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
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Text
import tv.plurx.app.ui.components.TvButton as Button
import tv.plurx.app.ui.components.TvTextButton as TextButton
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.tvFocusRing
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.movableContentOf
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
    val persistedLayout by settings.liveTvLayout.collectAsStateWithLifecycle(initialValue = null)
    val persistedMobileGuide by settings.liveTvMobileGuide.collectAsStateWithLifecycle(initialValue = null)
    var browse by remember { mutableStateOf(LiveTvBrowseView.List) }
    var tvLayout by remember { mutableStateOf(TvLiveLayout.GuidePreview) }
    var mobileGuideGrid by remember { mutableStateOf(false) }
    var scheduleChannelId by remember { mutableStateOf<String?>(null) }
    // The stored choice arrives asynchronously; adopt it once, then let taps
    // own the value. Re-adopting on every emission would fight the viewer.
    LaunchedEffect(persistedView) {
        persistedView?.let { browse = LiveTvBrowseView.fromStorage(it) }
    }
    LaunchedEffect(persistedLayout) { persistedLayout?.let { tvLayout = TvLiveLayout.fromStorage(it) } }
    LaunchedEffect(persistedMobileGuide) { persistedMobileGuide?.let { mobileGuideGrid = it == "grid" } }
    var favoritesOnly by remember { mutableStateOf(false) }
    var hideProtected by remember { mutableStateOf(false) }
    var detail by remember { mutableStateOf<Pair<LiveTvChannel, LiveTvProgramme>?>(null) }
    var overlayVisible by remember { mutableStateOf(true) }
    var temporaryGuide by remember { mutableStateOf(false) }
    var showingInfo by remember { mutableStateOf(false) }
    var showingMore by remember { mutableStateOf(false) }
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
    LaunchedEffect(state.playing) { if (!state.playing) fullscreen = false }
    // The contract's four seconds. Without this the overlay never hid at all,
    // so `ten-foot · overlay · idle → hide` was a row nothing implemented.
    LaunchedEffect(lastInteraction, overlayVisible, temporaryGuide, showingInfo, showingMore, state.playing, state.paused, isInPip) {
        if (!overlayVisible || temporaryGuide || showingInfo || showingMore || !state.playing || state.paused || isInPip) return@LaunchedEffect
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
    BackHandler(fullscreen) {
        when {
            temporaryGuide -> temporaryGuide = false
            showingInfo -> showingInfo = false
            showingMore -> showingMore = false
            overlayVisible -> overlayVisible = false
            else -> fullscreen = false
        }
    }

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
            LiveTvInputOutcome.ReturnBrowser -> {
                temporaryGuide = false
                fullscreen = false
            }
            LiveTvInputOutcome.ClosePanel -> {
                temporaryGuide = false
                showingInfo = false
                showingMore = false
                detail = null
            }
            LiveTvInputOutcome.Exit -> {
                controller.stop()
                onBack()
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
            LiveTvInputOutcome.Delegate,
            LiveTvInputOutcome.FocusControl,
            LiveTvInputOutcome.FocusCell,
            LiveTvInputOutcome.FocusPanel,
            LiveTvInputOutcome.Activate,
            -> return false
            LiveTvInputOutcome.StripPrev, LiveTvInputOutcome.StripNext,
            LiveTvInputOutcome.Tune, LiveTvInputOutcome.Ignore -> return false
        }
        return true
    }

    val surface = if (television) LiveTvInputSurface.TenFoot else LiveTvInputSurface.Touch
    fun inputState(): LiveTvInputState = when {
        !fullscreen -> LiveTvInputState.Browser
        temporaryGuide -> LiveTvInputState.TemporaryGuide
        showingInfo -> LiveTvInputState.StreamInfo
        showingMore -> LiveTvInputState.Menu
        detail != null -> LiveTvInputState.ProgrammeDetails
        overlayVisible -> LiveTvInputState.FullscreenControls
        else -> LiveTvInputState.FullscreenHidden
    }
    val playerSurface = remember(controller) {
        movableContentOf { LiveTvPlayerSurface(controller) }
    }

    val screenModifier = if (fullscreen || isInPip) {
        Modifier.fillMaxSize()
    } else {
        Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).padding(16.dp)
    }
    Column(
        screenModifier
            // Key spellings and the handler that installs them both live in
            // LiveTvKeyAdapter, so this screen can only speak the contract's
            // vocabulary. A press on a hidden overlay is consumed whatever it
            // decides, so the focus engine cannot move focus behind a picture
            // that is showing no chrome.
            .liveTvInputAdapter(enabled = fullscreen && state.playing && !isInPip) { input ->
                applyOutcome(LiveTvInputPolicy.route(surface, inputState(), input))
            },
    ) {
        if (!fullscreen && !isInPip) {
            TextButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) { Text("Back") }
            Text("Live TV", style = MaterialTheme.typography.headlineMedium)
        }
        if (!isInPip && !fullscreen) {
            Text(state.title, style = MaterialTheme.typography.titleMedium)
            Text(state.message)
        }
        if (television && !fullscreen && !isInPip) {
            TelevisionLiveTvBrowser(
                state = state,
                controller = controller,
                channels = visible,
                now = now,
                layout = tvLayout,
                browse = browse,
                search = search,
                favoritesOnly = favoritesOnly,
                hideProtected = hideProtected,
                playerSurface = playerSurface,
                onBrowse = { browse = it },
                onSearch = { search = it },
                onToggleFavorites = { favoritesOnly = !favoritesOnly },
                onToggleProtected = { hideProtected = !hideProtected },
                onLayout = { selected ->
                    tvLayout = selected
                    scope.launch { settings.saveLiveTvLayout(selected.storageValue) }
                },
                onReload = controller::refresh,
                onFullscreen = { fullscreen = true; overlayVisible = true; lastInteraction += 1 },
                onDetail = { channel, programme -> detail = channel to programme },
                onLeave = { controller.stop(); onBack() },
            )
        } else {
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
                playerSurface()
                if (fullscreen && overlayVisible && !isInPip) {
                    LiveTvOverlay(
                        channel = state.watching,
                        airing = state.watching?.let { controller.airing(it, now) } ?: LiveTvAiring(),
                        status = state.status,
                        neighbours = visible,
                        onSelect = { controller.requestChannel(it) },
                        paused = state.paused,
                        temporaryGuide = temporaryGuide,
                        showingInfo = showingInfo,
                        moreOpen = showingMore,
                        guide = state.guide,
                        now = now,
                        onGuide = { temporaryGuide = true; lastInteraction += 1 },
                        onChannels = { fullscreen = false },
                        onTogglePause = { controller.togglePause(); lastInteraction += 1 },
                        onInfo = { showingInfo = true; lastInteraction += 1 },
                        onMore = { showingMore = true; lastInteraction += 1 },
                        onDismissMore = { showingMore = false; lastInteraction += 1 },
                        layout = tvLayout,
                        onLayout = { selected ->
                            tvLayout = selected
                            scope.launch { settings.saveLiveTvLayout(selected.storageValue) }
                            showingMore = false
                            lastInteraction += 1
                        },
                        onFuture = { selected, programme -> detail = selected to programme },
                        onClosePanel = {
                            temporaryGuide = false
                            showingInfo = false
                            lastInteraction += 1
                        },
                        onStop = { controller.stop(); fullscreen = false },
                        onLeave = { controller.stop(); onBack() },
                    )
                }
            }
            if (!isInPip && !fullscreen) {
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
                    onInfo = { showingInfo = true },
                    onStop = { controller.stop() },
                )
            }
        } else if (!isInPip) {
            TextButton(onClick = { controller.stop() }) { Text("Stop / retry cleanup") }
        }
        if (!fullscreen && !isInPip) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                TextButton(enabled = !state.busy, onClick = controller::refresh) {
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
            if (browse == LiveTvBrowseView.Guide && !television &&
                (formFactor != FormFactor.Compact || mobileGuideGrid)
            ) {
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
                if (formFactor == FormFactor.Compact) {
                    TextButton(onClick = {
                        mobileGuideGrid = false
                        scope.launch { settings.saveLiveTvMobileGuide("schedule") }
                    }) { Text("Selected channel schedule") }
                }
            } else if (browse == LiveTvBrowseView.Guide && !television) {
                val selected = visible.firstOrNull { it.id == scheduleChannelId }
                    ?: state.watching?.takeIf { watching -> visible.any { it.id == watching.id } }
                    ?: visible.firstOrNull()
                LaunchedEffect(selected?.id) { scheduleChannelId = selected?.id }
                LiveTvMobileSchedule(
                    channel = selected,
                    channels = visible,
                    guide = state.guide,
                    now = now,
                    onChannel = { scheduleChannelId = it.id },
                    onAiring = { channel -> controller.watch(channel) },
                    onFuture = { channel, programme -> detail = channel to programme },
                    onGrid = {
                        mobileGuideGrid = true
                        scope.launch { settings.saveLiveTvMobileGuide("grid") }
                    },
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
    if (showingInfo && !fullscreen) {
        ModalBottomSheet(onDismissRequest = { showingInfo = false }) {
            Column(Modifier.fillMaxWidth().padding(16.dp)) {
                Text("Stream info", style = MaterialTheme.typography.titleMedium)
                state.watching?.let { LiveTvTechnicalDetails(it, state.status) }
                TextButton(onClick = { showingInfo = false }) { Text("Close") }
            }
        }
    }
}

@Composable
private fun LiveTvMobileSchedule(
    channel: LiveTvChannel?,
    channels: List<LiveTvChannel>,
    guide: LiveTvGuide?,
    now: Long,
    onChannel: (LiveTvChannel) -> Unit,
    onAiring: (LiveTvChannel) -> Unit,
    onFuture: (LiveTvChannel, LiveTvProgramme) -> Unit,
    onGrid: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var pickerOpen by remember { mutableStateOf(false) }
    Column(modifier) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Box {
                TextButton(onClick = { pickerOpen = true }) {
                    Text(channel?.title ?: "Choose channel")
                }
                DropdownMenu(expanded = pickerOpen, onDismissRequest = { pickerOpen = false }) {
                    channels.forEach { choice ->
                        DropdownMenuItem(
                            text = { Text(choice.title) },
                            onClick = { pickerOpen = false; onChannel(choice) },
                        )
                    }
                }
            }
            TextButton(onClick = onGrid) { Text("Grid") }
        }
        Text("Schedule from ${liveTvTime(now - now.mod(LiveTvGuideReducer.SLOT_SECONDS))}", style = MaterialTheme.typography.labelSmall)
        val programmes = channel?.let { LiveTvGuideReducer.channel(guide, it.id)?.programmes }.orEmpty()
            .filter { it.end > now - LiveTvGuideReducer.SLOT_SECONDS }
        LazyColumn(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            if (channel != null && programmes.isEmpty()) {
                item {
                    TextButton(onClick = { if (channel.watchable) onAiring(channel) }, enabled = channel.watchable) {
                        Text("No programme information · Watch live")
                    }
                }
            }
            items(programmes, key = { "${it.start}:${it.end}:${it.title}" }) { programme ->
                val airing = programme.start <= now && now < programme.end
                TextButton(onClick = {
                    if (airing && channel != null) onAiring(channel)
                    else if (channel != null) onFuture(channel, programme)
                }) {
                    Column(Modifier.fillMaxWidth()) {
                        Text("${liveTvTime(programme.start)}–${liveTvTime(programme.end)}")
                        Text(programme.title, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        if (airing) Text("On now", style = MaterialTheme.typography.labelSmall)
                    }
                }
            }
        }
    }
}

@Composable
private fun TelevisionLiveTvBrowser(
    state: LiveTvPlayerState,
    controller: LiveTvPlayer,
    channels: List<LiveTvChannel>,
    now: Long,
    layout: TvLiveLayout,
    browse: LiveTvBrowseView,
    search: String,
    favoritesOnly: Boolean,
    hideProtected: Boolean,
    playerSurface: @Composable () -> Unit,
    onBrowse: (LiveTvBrowseView) -> Unit,
    onSearch: (String) -> Unit,
    onToggleFavorites: () -> Unit,
    onToggleProtected: () -> Unit,
    onLayout: (TvLiveLayout) -> Unit,
    onReload: () -> Unit,
    onFullscreen: () -> Unit,
    onDetail: (LiveTvChannel, LiveTvProgramme) -> Unit,
    onLeave: () -> Unit,
) {
    var layoutOpen by remember { mutableStateOf(false) }
    var moreOpen by remember { mutableStateOf(false) }
    val guideFocus = remember { FocusRequester() }
    var focusedId by remember { mutableStateOf<String?>(state.watching?.id ?: channels.firstOrNull()?.id) }
    LaunchedEffect(channels, focusedId) {
        if (channels.none { it.id == focusedId }) focusedId = channels.firstOrNull()?.id
    }
    val focused = channels.firstOrNull { it.id == focusedId } ?: state.watching ?: channels.firstOrNull()
    val focusedAiring = focused?.let { controller.airing(it, now) } ?: LiveTvAiring()

    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        TextButton(
            onClick = { onBrowse(LiveTvBrowseView.Guide) },
            modifier = Modifier.focusRequester(guideFocus),
        ) { Text("Guide") }
        TextButton(onClick = { onBrowse(LiveTvBrowseView.List) }) { Text("On now") }
        TextButton(onClick = onToggleFavorites) {
            Text(if (favoritesOnly) "All channels" else "Favorites")
        }
        OutlinedTextField(
            value = search,
            onValueChange = onSearch,
            label = { Text("Search") },
            modifier = Modifier.width(260.dp).tvFocusRing(),
            singleLine = true,
        )
        Box {
            TextButton(onClick = { layoutOpen = true }) { Text("Layout") }
            DropdownMenu(expanded = layoutOpen, onDismissRequest = { layoutOpen = false }) {
                TvLiveLayout.entries.forEach { choice ->
                    DropdownMenuItem(
                        text = { Text(choice.label + if (choice == layout) " · Selected" else "") },
                        onClick = { layoutOpen = false; onLayout(choice) },
                    )
                }
            }
        }
        if (state.playing) TextButton(onClick = onFullscreen) { Text("Return to live") }
        Box {
            TextButton(onClick = { moreOpen = true }) { Text("More") }
            DropdownMenu(expanded = moreOpen, onDismissRequest = { moreOpen = false }) {
                DropdownMenuItem(text = { Text("Refresh channels") }, onClick = { moreOpen = false; onReload() })
                DropdownMenuItem(
                    text = { Text(if (hideProtected) "Show protected" else "Hide protected") },
                    onClick = { moreOpen = false; onToggleProtected() },
                )
                DropdownMenuItem(text = { Text("Stop") }, onClick = { moreOpen = false; controller.stop() })
                DropdownMenuItem(text = { Text("Leave Live TV") }, onClick = { moreOpen = false; onLeave() })
            }
        }
    }

    if (channels.isEmpty()) {
        Text("No matching channels. Clear search or filters.")
        return
    }

    val window = remember(now) { LiveTvGuideReducer.window(now, slots = 3) }
    val grid = remember(state.guide, channels, window, now) {
        LiveTvGuideReducer.gridLayout(
            guide = state.guide,
            channels = channels,
            window = window,
            now = now,
            pxPerSlot = LiveTvGridMetrics.slotWidth.value,
        )
    }
    val selectAiring: (LiveTvChannel) -> Unit = { channel ->
        focusedId = channel.id
        if (state.watching?.id == channel.id && state.playing) onFullscreen() else controller.watch(channel)
    }

    if (layout == TvLiveLayout.ChannelBrowser && browse == LiveTvBrowseView.List) {
        Row(Modifier.fillMaxSize(), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            LiveTvOnNowList(
                channels = channels,
                controller = controller,
                now = now,
                watchingChannelId = state.watching?.id,
                onFocused = { focusedId = it.id },
                onSelect = selectAiring,
                modifier = Modifier.weight(0.34f).fillMaxHeight(),
            )
            Column(Modifier.weight(0.66f).fillMaxHeight()) {
                Box(Modifier.fillMaxWidth().weight(0.58f).background(Color.Black)) { playerSurface() }
                LiveTvFocusedProgramme(focused, focusedAiring, state.status)
                focused?.let { channel ->
                    val upcoming = LiveTvGuideReducer.channel(state.guide, channel.id)?.programmes.orEmpty()
                        .filter { it.end > now }.take(4)
                    upcoming.forEach { programme ->
                        TextButton(onClick = {
                            if (programme.start <= now && now < programme.end) selectAiring(channel)
                            else onDetail(channel, programme)
                        }) {
                            Text("${liveTvTime(programme.start)} · ${programme.title}")
                        }
                    }
                }
            }
        }
    } else if (browse == LiveTvBrowseView.Guide && layout != TvLiveLayout.GuideOverlay) {
        Column(Modifier.fillMaxSize()) {
            Row(Modifier.fillMaxWidth().weight(0.34f), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                Column(Modifier.weight(1f)) { LiveTvFocusedProgramme(focused, focusedAiring, state.status) }
                Box(Modifier.weight(1f).fillMaxHeight().background(Color.Black)) { playerSurface() }
            }
            LiveTvGuideGrid(
                layout = grid,
                slots = remember(window) { LiveTvGuideReducer.gridSlots(window) },
                playingChannelId = state.watching?.id,
                onAiring = selectAiring,
                onFuture = onDetail,
                dpadNavigation = true,
                onFocus = { focusedId = it.id },
                onToolbarBoundary = { guideFocus.requestFocus() },
                modifier = Modifier.weight(0.66f),
            )
        }
    } else if (browse == LiveTvBrowseView.Guide) {
        Box(Modifier.fillMaxSize().background(Color.Black)) {
            playerSurface()
            Column(
                Modifier.fillMaxWidth().fillMaxHeight(0.55f).align(androidx.compose.ui.Alignment.BottomCenter)
                    .background(MaterialTheme.colorScheme.surface).padding(10.dp),
            ) {
                LiveTvFocusedProgramme(focused, focusedAiring, state.status)
                LiveTvGuideGrid(
                    layout = grid,
                    slots = remember(window) { LiveTvGuideReducer.gridSlots(window) },
                    playingChannelId = state.watching?.id,
                    onAiring = selectAiring,
                    onFuture = onDetail,
                    dpadNavigation = true,
                    onFocus = { focusedId = it.id },
                    onToolbarBoundary = { guideFocus.requestFocus() },
                    modifier = Modifier.weight(1f),
                )
            }
        }
    } else if (layout == TvLiveLayout.GuidePreview) {
        Column(Modifier.fillMaxSize()) {
            Row(Modifier.fillMaxWidth().weight(0.34f), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                Column(Modifier.weight(1f)) { LiveTvFocusedProgramme(focused, focusedAiring, state.status) }
                Box(Modifier.weight(1f).fillMaxHeight().background(Color.Black)) { playerSurface() }
            }
            LiveTvOnNowList(
                channels = channels,
                controller = controller,
                now = now,
                watchingChannelId = state.watching?.id,
                onFocused = { focusedId = it.id },
                onSelect = selectAiring,
                modifier = Modifier.weight(0.66f),
            )
        }
    } else {
        Box(Modifier.fillMaxSize().background(Color.Black)) {
            playerSurface()
            Column(
                Modifier.fillMaxWidth().fillMaxHeight(0.55f).align(androidx.compose.ui.Alignment.BottomCenter)
                    .background(MaterialTheme.colorScheme.surface).padding(10.dp),
            ) {
                LiveTvFocusedProgramme(focused, focusedAiring, state.status)
                LiveTvOnNowList(
                    channels = channels,
                    controller = controller,
                    now = now,
                    watchingChannelId = state.watching?.id,
                    onFocused = { focusedId = it.id },
                    onSelect = selectAiring,
                    modifier = Modifier.weight(1f),
                )
            }
        }
    }
}

@Composable
private fun LiveTvOnNowList(
    channels: List<LiveTvChannel>,
    controller: LiveTvPlayer,
    now: Long,
    watchingChannelId: String?,
    onFocused: (LiveTvChannel) -> Unit,
    onSelect: (LiveTvChannel) -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(modifier, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        items(channels, key = { it.id }) { channel ->
            LiveTvBrowserRow(
                channel = channel,
                airing = controller.airing(channel, now),
                watching = watchingChannelId == channel.id,
                onFocused = { onFocused(channel) },
                onSelect = { onSelect(channel) },
            )
        }
    }
}

@Composable
private fun LiveTvBrowserRow(
    channel: LiveTvChannel,
    airing: LiveTvAiring,
    watching: Boolean,
    onFocused: () -> Unit,
    onSelect: () -> Unit,
) {
    TextButton(
        onClick = onSelect,
        enabled = channel.watchable,
        modifier = Modifier.fillMaxWidth().onFocusChanged { if (it.isFocused) onFocused() },
    ) {
        Column(Modifier.fillMaxWidth()) {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(channel.title, style = MaterialTheme.typography.titleSmall)
                LiveTvFormatBadges(channel)
                if (watching) Text("Watching", style = MaterialTheme.typography.labelSmall)
            }
            Text(
                airing.now?.title ?: if (channel.watchable) "No programme information · Watch live" else "Protected · unavailable",
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            airing.progress?.let { progress -> LinearProgressIndicator(progress = { progress }, Modifier.fillMaxWidth()) }
            airing.next?.let { Text("Next: ${it.title}", style = MaterialTheme.typography.labelSmall) }
        }
    }
}

@Composable
private fun LiveTvFocusedProgramme(channel: LiveTvChannel?, airing: LiveTvAiring, status: LiveTvStatus?) {
    Text(airing.now?.title ?: channel?.guide_name ?: "Select a channel", style = MaterialTheme.typography.titleLarge)
    channel?.let {
        Text(it.title, style = MaterialTheme.typography.labelMedium)
        LiveTvFormatBadges(it)
        airing.now?.let { programme -> Text("${liveTvTime(programme.start)}–${liveTvTime(programme.end)}") }
        LiveTvTechnicalDetails(it, status?.takeIf { observed -> observed.channel?.id == null || observed.channel.id == it.id })
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
    onInfo: () -> Unit,
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
        channel?.let { LiveTvFormatBadges(it) }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = onTogglePause) { Text(if (paused) "Play live" else "Pause") }
            TextButton(onClick = onToggleMute) { Text(if (muted) "Unmute" else "Mute") }
            if (canUsePip) TextButton(onClick = onPip) { Text("Picture-in-picture") }
            TextButton(onClick = onInfo) { Text("Info") }
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
    status: LiveTvStatus?,
    neighbours: List<LiveTvChannel>,
    onSelect: (LiveTvChannel) -> Unit,
    paused: Boolean,
    temporaryGuide: Boolean,
    showingInfo: Boolean,
    moreOpen: Boolean,
    guide: LiveTvGuide?,
    now: Long,
    onGuide: () -> Unit,
    onChannels: () -> Unit,
    onTogglePause: () -> Unit,
    onInfo: () -> Unit,
    onMore: () -> Unit,
    onDismissMore: () -> Unit,
    layout: TvLiveLayout,
    onLayout: (TvLiveLayout) -> Unit,
    onFuture: (LiveTvChannel, LiveTvProgramme) -> Unit,
    onClosePanel: () -> Unit,
    onStop: () -> Unit,
    onLeave: () -> Unit,
) {
    val guideFocus = remember { FocusRequester() }
    RequestInitialFocus(guideFocus, enabled = !temporaryGuide && !showingInfo && !moreOpen)
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
            channel?.let {
                val summary = liveTvTechnicalSummary(it, status)
                if (summary.isNotEmpty()) {
                    Text(
                        summary,
                        style = MaterialTheme.typography.labelSmall,
                        color = Color.White.copy(alpha = 0.78f),
                    )
                }
            }
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
            if (temporaryGuide) {
                val window = remember(now) { LiveTvGuideReducer.window(now, slots = 3) }
                val layout = remember(guide, neighbours, window, now) {
                    LiveTvGuideReducer.gridLayout(
                        guide = guide,
                        channels = neighbours,
                        window = window,
                        now = now,
                        pxPerSlot = LiveTvGridMetrics.slotWidth.value,
                    )
                }
                Column(
                    Modifier.fillMaxWidth().fillMaxHeight(0.48f)
                        .background(MaterialTheme.colorScheme.surface).padding(8.dp),
                ) {
                    Text("Guide", style = MaterialTheme.typography.titleMedium)
                    LiveTvGuideGrid(
                        layout = layout,
                        slots = remember(window) { LiveTvGuideReducer.gridSlots(window) },
                        playingChannelId = channel?.id,
                        onAiring = onSelect,
                        onFuture = onFuture,
                        dpadNavigation = true,
                        onToolbarBoundary = { guideFocus.requestFocus() },
                        modifier = Modifier.weight(1f),
                    )
                    TextButton(onClick = onClosePanel) { Text("Close guide") }
                }
            } else if (showingInfo) {
                Column(
                    Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surface).padding(12.dp),
                ) {
                    Text("Stream info", style = MaterialTheme.typography.titleMedium)
                    channel?.let { LiveTvTechnicalDetails(it, status) }
                    TextButton(onClick = onClosePanel) { Text("Close") }
                }
            }
            LinearProgressIndicator(
                progress = { airing.progress ?: 0f },
                modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                TextButton(onClick = onGuide, modifier = Modifier.focusRequester(guideFocus)) {
                    Text("Guide", color = Color.White)
                }
                TextButton(onClick = onChannels) { Text("Channels", color = Color.White) }
                TextButton(onClick = onTogglePause) {
                    Text(if (paused) "Play live" else "Pause", color = Color.White)
                }
                TextButton(onClick = onInfo) { Text("Info", color = Color.White) }
                Box {
                    TextButton(onClick = onMore) { Text("More", color = Color.White) }
                    DropdownMenu(expanded = moreOpen, onDismissRequest = onDismissMore) {
                        TvLiveLayout.entries.forEach { choice ->
                            DropdownMenuItem(
                                text = {
                                    Text(
                                        "Layout: ${choice.label}" +
                                            if (choice == layout) " · Selected" else "",
                                    )
                                },
                                onClick = { onLayout(choice) },
                            )
                        }
                        DropdownMenuItem(
                            text = { Text("Stop") },
                            onClick = { onDismissMore(); onStop() },
                        )
                        DropdownMenuItem(
                            text = { Text("Leave Live TV") },
                            onClick = { onDismissMore(); onLeave() },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun LiveTvPlayerSurface(controller: LiveTvPlayer) {
    AndroidView(
        factory = { context ->
            PlayerView(context).apply {
                useController = false
                layoutParams = ViewGroup.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT,
                    ViewGroup.LayoutParams.MATCH_PARENT,
                )
                player = controller.player
            }
        },
        update = { view -> view.player = controller.player },
        modifier = Modifier.fillMaxSize().background(Color.Black),
    )
}

@Composable
private fun LiveTvTechnicalDetails(channel: LiveTvChannel, status: LiveTvStatus?) {
    Column(
        Modifier.fillMaxWidth().padding(vertical = 6.dp),
        verticalArrangement = Arrangement.spacedBy(5.dp),
    ) {
        channel.sourceFormatDescription?.let { LiveTvTechnicalRow("Source", it) }
        channel.measuredSource?.observed_at?.let { observed ->
            LiveTvTechnicalRow("Observed", liveTvObservedTime(observed))
        }
        status?.let {
            val delivery = buildList {
                add("H.264")
                it.output_height?.let { height -> add("${height}p") }
                add("AAC")
                it.encoder?.takeUnless { encoder -> encoder == "pending" }
                    ?.let { encoder -> add("${encoder.uppercase()} encoder") }
            }.joinToString(" · ")
            LiveTvTechnicalRow("Playing", delivery)
        }
        val meters = status?.signal?.let { signal ->
            listOfNotNull(
                signal.strength_percent?.let { "Strength" to it },
                signal.quality_percent?.let { "Quality" to it },
                signal.symbol_quality_percent?.let { "Symbol" to it },
            )
        }.orEmpty()
        if (meters.isNotEmpty()) {
            Text(
                "SIGNAL",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                meters.forEach { (label, value) ->
                    Column(Modifier.weight(1f)) {
                        Text("$label $value%", style = MaterialTheme.typography.labelSmall)
                        LinearProgressIndicator(
                            progress = { value / 100f },
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                }
            }
        }
    }
}

private fun liveTvObservedTime(unixSeconds: Long): String =
    java.text.DateFormat.getDateTimeInstance(java.text.DateFormat.MEDIUM, java.text.DateFormat.SHORT)
        .format(java.util.Date(unixSeconds * 1000))

@Composable
private fun LiveTvTechnicalRow(label: String, value: String) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            label.uppercase(),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            value,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

private fun liveTvTechnicalSummary(channel: LiveTvChannel, status: LiveTvStatus?): String {
    val signal = status?.signal
    val reception = listOfNotNull(
        signal?.strength_percent?.let { "strength $it%" },
        signal?.quality_percent?.let { "quality $it%" },
        signal?.symbol_quality_percent?.let { "symbol $it%" },
    ).joinToString(" · ")
    return listOfNotNull(channel.sourceFormatDescription, reception.takeIf { it.isNotEmpty() })
        .joinToString(" · ")
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
