@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.livetv

import tv.plurx.app.player.PlaybackInfoPanel
import tv.plurx.app.player.PlaybackInfoFact
import tv.plurx.app.player.PlaybackStatsMode
import tv.plurx.app.player.playerStateLabel
import tv.plurx.app.player.playbackInfoExplanation

import android.Manifest
import android.app.PictureInPictureParams
import android.content.pm.PackageManager
import android.os.Build
import android.util.Rational
import android.view.ViewGroup
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.LocalActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.util.Consumer
import androidx.core.app.PictureInPictureModeChangedInfo
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.PictureInPictureAlt
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.VolumeOff
import androidx.compose.material.icons.filled.VolumeUp
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Text
import tv.plurx.app.ui.components.TvIconButton
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
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.Alignment
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.media3.ui.PlayerView
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import tv.plurx.app.data.Session
import tv.plurx.app.data.SettingsStore
import tv.plurx.app.reminders.ReminderAlarms
import tv.plurx.app.ui.components.safeDisplayInsets
import tv.plurx.app.ui.FormFactor
import tv.plurx.app.ui.currentFormFactor
import androidx.compose.foundation.layout.windowInsetsPadding
import kotlinx.coroutines.delay

@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
fun LiveTvScreen(
    origin: String,
    dvrController: DvrController? = null,
    /** A channel a reminder notification asked for; tuned once, when it arrives. */
    initialChannelId: String? = null,
    onOpenItem: (Long) -> Unit = {},
    onOpenRecording: (String) -> Unit = {},
    onOpenRecordingActivity: () -> Unit = {},
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    val controller = LiveTvPlayer.get(context)
    val state by controller.state.collectAsStateWithLifecycle()
    var search by remember { mutableStateOf("") }
    var searchOpen by remember { mutableStateOf(false) }
    var fullscreen by remember { mutableStateOf(false) }
    val settings = remember(context) { SettingsStore(context) }
    val persistedView by settings.liveTvView.collectAsStateWithLifecycle(initialValue = null)
    val persistedLayout by settings.liveTvLayout.collectAsStateWithLifecycle(initialValue = null)
    val persistedMobileGuide by settings.liveTvMobileGuide.collectAsStateWithLifecycle(initialValue = null)
    var browse by remember { mutableStateOf(LiveTvBrowseView.List) }
    var tvLayout by remember { mutableStateOf(TvLiveLayout.GuidePreview) }
    var mobileGuideGrid by remember { mutableStateOf(false) }
    var scheduleChannelId by remember { mutableStateOf<String?>(null) }
    var guideWindowStart by remember {
        val current = System.currentTimeMillis() / 1000
        mutableLongStateOf(current - current.mod(LiveTvGuideReducer.SLOT_SECONDS))
    }
    var tvFocusedChannelId by remember { mutableStateOf<String?>(null) }
    var guideFocusTarget by remember { mutableStateOf<LiveTvGuideFocusTarget?>(null) }
    var guideAnchorTime by remember { mutableStateOf<Long?>(null) }
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
    val formFactor = currentFormFactor()
    val television = formFactor == FormFactor.Television
    val wideBrowser = formFactor != FormFactor.Compact
    // `backFocus` is attached to the phone nav bar's Back button, which no
    // longer exists on television. Asking for it there is a silent no-op that
    // leaves the guide layouts opening with nothing focused; the television
    // browser seeds its own toolbar instead.
    RequestInitialFocus(backFocus, enabled = !television)
    val activity = LocalActivity.current
    DisposableEffect(controller, activity) {
        activity?.let(controller::bindDisplayMode)
        onDispose { activity?.let(controller::unbindDisplayMode) }
    }
    val componentActivity = activity as? ComponentActivity
    val canUsePip = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
        context.packageManager.hasSystemFeature(PackageManager.FEATURE_PICTURE_IN_PICTURE)

    // Key on the token as well: `Session.token` is a plain global, not Compose
    // state, so a profile switch that keeps this screen composed would leave
    // the application-scoped controller heartbeating the previous profile's
    // capability under the new profile's session.
    val token = Session.token.orEmpty()
    LaunchedEffect(origin, token) { controller.load(origin, token) }

    // The DVR is a second, independent surface over the same profile. It never
    // gates the lineup, the guide or a start: an unreachable DVR leaves every
    // Live TV control working and simply draws no marks.
    val dvr = dvrController
    val dvrState by remember(dvr) {
        dvr?.state ?: MutableStateFlow(DvrScreenState()).asStateFlow()
    }.collectAsStateWithLifecycle()
    var recordingsOpen by remember { mutableStateOf(false) }
    var dvrChip by remember { mutableStateOf(DvrChip.Upcoming) }
    var stopRecordingFor by remember { mutableStateOf<Pair<String, String>?>(null) }
    var deleteFileFor by remember { mutableStateOf<String?>(null) }
    var reminderNow by remember { mutableLongStateOf(now) }
    var initialChannelTuned by remember(initialChannelId) { mutableStateOf(false) }
    val notificationPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { /* Denial is benign: the in-app overlay is the surface that always works. */ }
    LaunchedEffect(dvr) {
        dvr?.load()
        dvr?.startDuePoll()
    }
    DisposableEffect(dvr) {
        dvr?.setHighFrequency("live-tv", true)
        onDispose { dvr?.setHighFrequency("live-tv", false) }
    }
    // Seed the visible guide on entry/load. The profile controller also
    // invalidates it after mutations, active-ID changes and the 30-second cap.
    LaunchedEffect(dvr, state.guide?.fetched_at) { dvr?.refreshMarks() }
    LaunchedEffect(recordingsOpen, dvrChip) {
        if (!recordingsOpen) return@LaunchedEffect
        when (dvrChip) {
            DvrChip.Saved -> dvr?.refreshLibrary()
            DvrChip.Attention -> dvr?.refreshAttention()
            DvrChip.Rules -> dvr?.refreshRules()
            DvrChip.Upcoming, DvrChip.Skipped, DvrChip.Reminders, DvrChip.Manual ->
                dvr?.refreshMarks()
        }
    }
    /**
     * Ask for notifications at the moment a reminder is set, not at launch: a
     * permission prompt on the way into Live TV is a prompt for something the
     * viewer has not asked for yet.
     */
    fun remind(channelId: String, airingStart: Long) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
        dvr?.remind(channelId, airingStart)
    }

    /**
     * The second press of Record series: the rules list, open on the rule the
     * server has just made. A rule created from a title-only guide is locked to
     * one channel and keeps everything, and both are decisions worth changing
     * before the next episode airs rather than after it.
     */
    fun recordSeries(channelId: String, airingStart: Long) {
        dvr?.recordSeries(channelId, airingStart) {
            recordingsOpen = true
            dvrChip = DvrChip.Rules
        }
    }

    /**
     * One verb for four meanings, exactly as the route has. The only one that
     * cannot be a single press is the last: a finished recording answers
     * `delete_file_required`, and the file goes only after the viewer has said
     * in a second, separate press that they meant the file too.
     */
    fun stopRecording(id: String) {
        val row = (dvrState.schedule + dvrState.library).firstOrNull { it.id == id }
        val activeTitle = row?.takeIf { it.recording }?.title
            ?: dvrState.overview?.active?.firstOrNull { it.recording_id == id }?.title
        if (activeTitle != null) {
            stopRecordingFor = id to activeTitle
        } else {
            dvr?.stop(id) { needsConfirmation -> deleteFileFor = needsConfirmation }
        }
    }
    // A channel replacement deliberately detaches the old decoder while busy.
    // That transient `playing = false` must not throw the viewer out of the
    // fullscreen guide; only a completed stop or terminal failure closes it.
    LaunchedEffect(state.playing, state.busy) {
        if (!state.playing && !state.busy) fullscreen = false
    }
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
            controller.expireSourceFormats(now)
        }
    }
    // A second-by-second clock, and only while a reminder is on screen. The
    // screen's own thirty-second tick is right for "until 9:00" and wrong for a
    // bar that has to reach its end exactly when the programme starts.
    LaunchedEffect(dvrState.due.isEmpty()) {
        if (dvrState.due.isEmpty()) return@LaunchedEffect
        while (true) {
            val at = System.currentTimeMillis() / 1000
            reminderNow = at
            dvr?.expireDue(at)
            delay(1_000)
        }
    }
    // A television has no reminder notifications worth posting, so it sets no
    // alarms; the overlay is the whole story there.
    LaunchedEffect(dvrState.reminders, television) {
        if (!television) ReminderAlarms.mirror(context, dvrState.reminders)
    }
    LaunchedEffect(initialChannelId, state.channels) {
        if (initialChannelTuned || initialChannelId.isNullOrEmpty()) return@LaunchedEffect
        val target = state.channels.firstOrNull { it.id == initialChannelId } ?: return@LaunchedEffect
        initialChannelTuned = true
        controller.watch(target)
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
    val guidePageSpan = LiveTvGridMetrics.TELEVISION_VISIBLE_SLOTS *
        LiveTvGuideReducer.SLOT_SECONDS
    fun guidePage(delta: Int): Long {
        val available = state.guide?.window ?: return guideWindowStart
        val latest = maxOf(available.start, available.end - guidePageSpan)
        return (guideWindowStart + delta * guidePageSpan).coerceIn(available.start, latest)
    }
    fun guideNow(): Long {
        val current = now - now.mod(LiveTvGuideReducer.SLOT_SECONDS)
        val available = state.guide?.window ?: return current
        val latest = maxOf(available.start, available.end - guidePageSpan)
        return current.coerceIn(available.start, latest)
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

    val touchRecordingsPanel: @Composable (Modifier) -> Unit = { panelModifier ->
        DvrRecordingsPanel(
            dvr = dvrState,
            now = now,
            chip = dvrChip,
            onChip = { dvrChip = it },
            onOpenItem = onOpenItem,
            onOpenRecording = onOpenRecording,
            onMoreLibrary = { dvr?.refreshLibrary(more = true) },
            onMoreAttention = { dvr?.refreshAttention(more = true) },
            onMoreUpcoming = { dvr?.refreshSchedule(more = true) },
            onStop = ::stopRecording,
            onRestore = { id -> dvr?.restore(id) },
            onRuleEnabled = { id, enabled -> dvr?.setRuleEnabled(id, enabled) },
            onRuleNewOnly = { id, newOnly -> dvr?.setRuleNewOnly(id, newOnly) },
            onRuleDelete = { id -> dvr?.deleteRule(id) },
            onRuleMove = { id, delta -> dvr?.moveRule(id, delta) },
            onForgetReminder = { id -> dvr?.forgetReminder(id) },
            onManual = { channel, start, end, title ->
                dvr?.recordManual(channel, start, end, title)
            },
            modifier = panelModifier,
        )
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
                val handled = applyOutcome(LiveTvInputPolicy.route(surface, inputState(), input))
                if (!handled && fullscreen && overlayVisible) lastInteraction += 1
                handled
            },
    ) {
        // The Back button, the headline, the title line and the status line
        // were four bands of chrome above the content. The phone gets a nav
        // bar; the television gets one 24 dp toolbar inside the browser.
        if (!television && !fullscreen && !isInPip) {
            LiveTvPhoneTopBar(
                onBack = onBack,
                backFocus = backFocus,
                onSearch = { searchOpen = true },
                onRefresh = controller::refresh,
                hideProtected = hideProtected,
                onToggleProtected = { hideProtected = !hideProtected },
                cleanupUnconfirmed = state.message.contains("leanup", ignoreCase = false),
                onRetryCleanup = { controller.stop() },
                onLeave = { controller.stop(); onBack() },
            )
        }
        if (wideBrowser && !fullscreen && !isInPip) {
            WideLiveTvBrowser(
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
                onBrowse = { selected ->
                    browse = selected
                    scope.launch { settings.saveLiveTvView(selected.storage) }
                },
                onSearch = { search = it },
                onToggleFavorites = { favoritesOnly = !favoritesOnly },
                onToggleProtected = { hideProtected = !hideProtected },
                onLayout = { selected ->
                    tvLayout = selected
                    scope.launch { settings.saveLiveTvLayout(selected.storageValue) }
                },
                guideWindowStart = guideWindowStart,
                onGuideEarlier = { guideWindowStart = guidePage(-1) },
                onGuideNow = { guideWindowStart = guideNow() },
                onGuideLater = { guideWindowStart = guidePage(1) },
                canGuideEarlier = guidePage(-1) != guideWindowStart,
                canGuideLater = guidePage(1) != guideWindowStart,
                focusedChannelId = tvFocusedChannelId,
                guideFocusTarget = guideFocusTarget,
                guideAnchorTime = guideAnchorTime,
                onFocusedChannel = { tvFocusedChannelId = it.id },
                onGuideNavigation = { target, anchor ->
                    guideFocusTarget = target
                    guideAnchorTime = anchor
                    tvFocusedChannelId = target.channelId
                },
                onReload = controller::refresh,
                onClearFilters = {
                    search = ""
                    favoritesOnly = false
                    hideProtected = false
                },
                onFullscreen = { fullscreen = true; overlayVisible = true; lastInteraction += 1 },
                onFullscreenGuide = {
                    fullscreen = true
                    temporaryGuide = true
                    overlayVisible = true
                    lastInteraction += 1
                },
                onDetail = { channel, programme -> detail = channel to programme },
                onLeave = { controller.stop(); onBack() },
                dvr = dvrState,
                recordingsOpen = recordingsOpen && television,
                onRecordings = { recordingsOpen = it },
                dvrChip = dvrChip,
                onDvrChip = { dvrChip = it },
                onOpenItem = onOpenItem,
                onOpenRecording = onOpenRecording,
                onMoreLibrary = { dvr?.refreshLibrary(more = true) },
                onMoreAttention = { dvr?.refreshAttention(more = true) },
                onMoreUpcoming = { dvr?.refreshSchedule(more = true) },
                onOpenRecordingActivity = onOpenRecordingActivity,
                onRecord = { channel, programme -> dvr?.record(channel.id, programme.start) },
                onRecordSeries = { channel, programme -> recordSeries(channel.id, programme.start) },
                onRemind = { channel, programme -> remind(channel.id, programme.start) },
                onStopRecording = ::stopRecording,
                onForgetReminder = { id -> dvr?.forgetReminder(id) },
                onRestore = { id -> dvr?.restore(id) },
                onRuleEnabled = { id, enabled -> dvr?.setRuleEnabled(id, enabled) },
                onRuleNewOnly = { id, newOnly -> dvr?.setRuleNewOnly(id, newOnly) },
                onRuleDelete = { id -> dvr?.deleteRule(id) },
                onRuleMove = { id, delta -> dvr?.moveRule(id, delta) },
                onManual = { channel, start, end, title ->
                    dvr?.recordManual(channel, start, end, title)
                },
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
                if (!fullscreen && !isInPip) {
                    // The chips live on the picture. Six lines of `nowBar`
                    // under it left one list row on an iPhone-sized screen.
                    Column(
                        Modifier.align(Alignment.TopStart).padding(10.dp)
                            .background(Color(0x99000000), MaterialTheme.shapes.small)
                            .padding(horizontal = 8.dp, vertical = 4.dp),
                    ) {
                        Text(
                            "● LIVE · ${state.watching?.guide_number.orEmpty()} " +
                                state.watching?.guide_name.orEmpty(),
                            style = MaterialTheme.typography.labelSmall,
                            color = Color.White,
                        )
                        state.watching?.let { watching ->
                            dvrRecordingContext(dvrState, watching, controller.airing(watching, now).now, now)
                                ?.let { Text("● $it", style = MaterialTheme.typography.labelSmall, color = Color.White) }
                        }
                    }
                    Row(
                        Modifier.align(Alignment.TopEnd).padding(6.dp),
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        if (canUsePip) {
                            TvIconButton(onClick = ::enterPip, modifier = Modifier.size(30.dp)) {
                                Icon(
                                    Icons.Filled.PictureInPictureAlt,
                                    contentDescription = "Picture-in-picture",
                                    tint = Color.White,
                                )
                            }
                        }
                        TvIconButton(
                            onClick = { fullscreen = true },
                            modifier = Modifier.size(30.dp),
                        ) {
                            Icon(
                                Icons.Filled.Fullscreen,
                                contentDescription = "Fullscreen",
                                tint = Color.White,
                            )
                        }
                    }
                    LinearProgressIndicator(
                        progress = {
                            state.watching?.let { controller.airing(it, now).progress } ?: 0f
                        },
                        modifier = Modifier
                            .align(Alignment.BottomCenter)
                            .fillMaxWidth()
                            .height(3.dp),
                    )
                }
                if (fullscreen && overlayVisible && !isInPip) {
                    LiveTvOverlay(
                        player = controller.player,
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
                        dvr = dvrState,
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
                        guideWindowStart = guideWindowStart,
                        onGuideNow = { guideWindowStart = guideNow() },
                        onGuideEarlier = { guideWindowStart = guidePage(-1) },
                        onGuideLater = { guideWindowStart = guidePage(1) },
                        canGuideEarlier = guidePage(-1) != guideWindowStart,
                        canGuideLater = guidePage(1) != guideWindowStart,
                        guideFocusTarget = guideFocusTarget,
                        guideAnchorTime = guideAnchorTime,
                        onGuideNavigation = { target, anchor ->
                            guideFocusTarget = target
                            guideAnchorTime = anchor
                            tvFocusedChannelId = target.channelId
                        },
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
                LiveTvPhoneCaption(
                    channel = state.watching,
                    airing = state.watching?.let { controller.airing(it, now) } ?: LiveTvAiring(),
                    now = now,
                    muted = state.muted,
                    paused = state.paused,
                    onTogglePause = controller::togglePause,
                    onToggleMute = controller::toggleMute,
                    onInfo = { showingInfo = true },
                    onStop = { controller.stop() },
                )
            }
        } else if (!isInPip) {
            TextButton(onClick = { controller.stop() }) { Text("Stop / retry cleanup") }
        }
        if (!fullscreen && !isInPip) {
            // One 48 dp toolbar: On now · Guide · Favorites, and the count.
            // The FlowRow of four buttons and the always-visible search field
            // were another 130 dp of chrome above a one-row list.
            LiveTvStatusLine(state.message)
            LiveTvPhoneToolbar(
                browse = browse,
                favoritesOnly = favoritesOnly,
                recordings = recordingsOpen,
                onRecordings = { recordingsOpen = true },
                summary = buildString {
                    append("${state.channels.size} channels")
                    dvrState.indicatorLabel()?.let { append(" · $it") }
                    if (state.guide?.freshness != null && state.guide?.freshness != "fresh") {
                        append(if (state.guide?.freshness == "stale") " · guide is stale" else " · no guide data")
                    }
                },
                onBrowse = { selected, favorites ->
                    // Favorites is a filter, not a saved browse view: only a
                    // real On now / Guide choice writes `liveTvView`.
                    val changed = selected != browse
                    browse = selected
                    favoritesOnly = favorites
                    recordingsOpen = false
                    if (changed && !favorites) {
                        scope.launch { settings.saveLiveTvView(selected.storage) }
                    }
                },
            )
            dvrState.indicatorLabel()?.let { label ->
                TextButton(onClick = onOpenRecordingActivity) { Text("● $label · Activity") }
            }
            if (recordingsOpen) {
                // Recordings is not a saved browse view: On now and Guide are a
                // habit, and a viewer who last checked the schedule did not ask
                // for Live TV to open there next time.
                touchRecordingsPanel(Modifier.weight(1f))
            } else if (browse == LiveTvBrowseView.Guide && !television &&
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
                    marks = dvrState.marks,
                    now = now,
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
                    verticalArrangement = Arrangement.spacedBy(2.dp),
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
    if (searchOpen) {
        // The shape Apple already had: a dialog the viewer asks for, not a
        // 260 dp text field standing in the toolbar forever.
        AlertDialog(
            onDismissRequest = { searchOpen = false },
            title = { Text("Find a channel") },
            text = {
                OutlinedTextField(
                    search,
                    onValueChange = { search = it },
                    label = { Text("Number, name, or what is on") },
                    modifier = Modifier.fillMaxWidth().testTag("live-tv-channel-search").tvFocusRing(),
                    singleLine = true,
                )
            },
            confirmButton = { TextButton(onClick = { searchOpen = false }) { Text("Done") } },
            dismissButton = {
                TextButton(onClick = { search = ""; searchOpen = false }) { Text("Clear") }
            },
        )
    }
    if (wideBrowser && !television && recordingsOpen) {
        ModalBottomSheet(onDismissRequest = { recordingsOpen = false }) {
            TextButton(onClick = { recordingsOpen = false }) { Text("Close recordings") }
            touchRecordingsPanel(Modifier.fillMaxWidth().fillMaxHeight(0.85f))
        }
    }
    detail?.let { (channel, programme) ->
        ModalBottomSheet(onDismissRequest = { detail = null }) {
            LiveTvProgrammeDetail(
                channel,
                programme,
                actions = {
                    DvrCellActions(
                        channel = channel,
                        programme = programme,
                        dvr = dvrState,
                        now = now,
                        onWatch = { detail = null; controller.watch(channel) },
                        onRecord = { dvr?.record(channel.id, programme.start) },
                        onRecordSeries = { detail = null; recordSeries(channel.id, programme.start) },
                        onRemind = { remind(channel.id, programme.start) },
                        onStop = ::stopRecording,
                        onForgetReminder = { id -> dvr?.forgetReminder(id) },
                    )
                },
            ) { detail = null }
        }
    }
    stopRecordingFor?.let { (id, title) ->
        AlertDialog(
            onDismissRequest = { stopRecordingFor = null },
            title = { Text("Stop recording $title?") },
            text = { Text("Any captured portion will be kept. Watching continues.") },
            confirmButton = {
                TextButton(onClick = {
                    stopRecordingFor = null
                    dvr?.stop(id) { needsConfirmation -> deleteFileFor = needsConfirmation }
                }) { Text("Stop recording") }
            },
            dismissButton = {
                TextButton(onClick = { stopRecordingFor = null }) { Text("Keep recording") }
            },
        )
    }
    deleteFileFor?.let { id ->
        AlertDialog(
            onDismissRequest = { deleteFileFor = null },
            title = { Text("Delete this recording?") },
            text = { Text("The recording and its file are removed. This cannot be undone.") },
            confirmButton = {
                TextButton(onClick = {
                    deleteFileFor = null
                    dvr?.stop(id, deleteFile = true)
                }) { Text("Delete file") }
            },
            dismissButton = {
                TextButton(onClick = { deleteFileFor = null }) { Text("Keep") }
            },
        )
    }
    dvrState.due.firstOrNull()?.let { reminder ->
        // Lower-left, over whatever is on screen — and a sibling of the screen's
        // own column rather than a child of it, so it is not a row that pushes
        // the guide up by its own height.
        Box(
            Modifier
                .fillMaxSize()
                .windowInsetsPadding(safeDisplayInsets())
                .padding(16.dp),
        ) {
            ReminderOverlay(
                reminder = reminder,
                now = reminderNow,
                onWatch = {
                    dvr?.acknowledge(reminder.id)
                    state.channels.firstOrNull { it.id == reminder.channel_id }
                        ?.let { controller.watch(it) }
                },
                onRecord = { dvr?.record(reminder.channel_id, reminder.airing_start) },
                onDismiss = { dvr?.acknowledge(reminder.id) },
                modifier = Modifier.align(Alignment.BottomStart),
            )
        }
    }
    if (showingInfo && !fullscreen) {
        ModalBottomSheet(onDismissRequest = { showingInfo = false }) {
            Column(Modifier.fillMaxWidth().padding(16.dp)) {
                Text("Stream info", style = MaterialTheme.typography.titleMedium)
                state.watching?.let { LiveTvPlaybackInformation(it, state.status, controller.player, { showingInfo = false }, Modifier.fillMaxWidth().heightIn(max = 760.dp)) }
                TextButton(onClick = { showingInfo = false }) { Text("Close") }
            }
        }
    }
}

/** 64 dp of nav bar: the title, search, and everything else in one menu. */
@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
private fun LiveTvPhoneTopBar(
    onBack: () -> Unit,
    backFocus: FocusRequester,
    onSearch: () -> Unit,
    onRefresh: () -> Unit,
    hideProtected: Boolean,
    onToggleProtected: () -> Unit,
    cleanupUnconfirmed: Boolean,
    onRetryCleanup: () -> Unit,
    onLeave: () -> Unit,
) {
    var moreOpen by remember { mutableStateOf(false) }
    Row(
        Modifier.fillMaxWidth().height(64.dp).padding(horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        TextButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) { Text("Back") }
        Text("Live TV", style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.weight(1f))
        TvIconButton(onClick = onSearch) {
            Icon(Icons.Filled.Search, contentDescription = "Search channels")
        }
        Box {
            TvIconButton(onClick = { moreOpen = true }) {
                Icon(Icons.Filled.MoreVert, contentDescription = "More")
            }
            DropdownMenu(expanded = moreOpen, onDismissRequest = { moreOpen = false }) {
                DropdownMenuItem(
                    text = { Text("Refresh channels") },
                    onClick = { moreOpen = false; onRefresh() },
                )
                DropdownMenuItem(
                    text = { Text(if (hideProtected) "Show protected" else "Hide protected") },
                    onClick = { moreOpen = false; onToggleProtected() },
                )
                if (cleanupUnconfirmed) {
                    DropdownMenuItem(
                        text = { Text("Retry cleanup") },
                        onClick = { moreOpen = false; onRetryCleanup() },
                    )
                }
                DropdownMenuItem(
                    text = { Text("Leave Live TV") },
                    onClick = { moreOpen = false; onLeave() },
                )
            }
        }
    }
}

/** One 56 dp caption line where the six-line now bar used to be. */
@Composable
private fun LiveTvPhoneCaption(
    channel: LiveTvChannel?,
    airing: LiveTvAiring,
    now: Long,
    muted: Boolean,
    paused: Boolean,
    onTogglePause: () -> Unit,
    onToggleMute: () -> Unit,
    onInfo: () -> Unit,
    onStop: () -> Unit,
) {
    val detail = listOfNotNull(
        airing.now?.let { "${liveTvTime(it.start)}–${liveTvTime(it.end)}" },
        airing.now?.takeIf { it.end > now }?.let { "${(it.end - now) / 60} min left" },
        airing.next?.let { "Next: ${it.title}" },
    ).joinToString(" · ")
    Row(
        Modifier.fillMaxWidth().height(56.dp).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Column(Modifier.weight(1f)) {
            Text(
                airing.now?.title ?: channel?.guide_name ?: "Live television",
                style = MaterialTheme.typography.titleSmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                detail,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        TvIconButton(onClick = onTogglePause, modifier = Modifier.size(32.dp)) {
            Icon(
                if (paused) Icons.Filled.PlayArrow else Icons.Filled.Pause,
                contentDescription = if (paused) "Play live" else "Pause",
            )
        }
        TvIconButton(onClick = onToggleMute, modifier = Modifier.size(32.dp)) {
            Icon(
                if (muted) Icons.Filled.VolumeOff else Icons.Filled.VolumeUp,
                contentDescription = if (muted) "Unmute" else "Mute",
            )
        }
        TvIconButton(onClick = onInfo, modifier = Modifier.size(32.dp)) {
            Icon(Icons.Filled.Info, contentDescription = "Stream info")
        }
        TvIconButton(onClick = onStop, modifier = Modifier.size(32.dp)) {
            Icon(Icons.Filled.Stop, contentDescription = "Stop")
        }
    }
}

/** 48 dp: On now · Guide · Recordings · Favorites, and what the lineup holds. */
@Composable
private fun LiveTvPhoneToolbar(
    browse: LiveTvBrowseView,
    favoritesOnly: Boolean,
    recordings: Boolean,
    onRecordings: () -> Unit,
    summary: String,
    onBrowse: (LiveTvBrowseView, Boolean) -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().height(48.dp).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        LiveTvSegment("On now", !recordings && browse == LiveTvBrowseView.List && !favoritesOnly) {
            onBrowse(LiveTvBrowseView.List, false)
        }
        LiveTvSegment("Guide", !recordings && browse == LiveTvBrowseView.Guide) {
            onBrowse(LiveTvBrowseView.Guide, favoritesOnly)
        }
        LiveTvSegment("Recordings", recordings, onClick = onRecordings)
        LiveTvSegment("Favorites", !recordings && favoritesOnly) {
            onBrowse(LiveTvBrowseView.List, true)
        }
        Spacer(Modifier.weight(1f))
        Text(
            summary,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun LiveTvSegment(
    label: String,
    active: Boolean,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    val television = currentFormFactor() == FormFactor.Television
    TextButton(onClick = onClick, compact = true, modifier = modifier) {
        Text(
            label,
            // 13 sp is the phone's segmented-control size; a television row is
            // 28 dp tall and reads 11 sp from ten feet.
            fontSize = if (television) 11.sp else 13.sp,
            color = if (active) {
                MaterialTheme.colorScheme.primary
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
        )
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
        // The channel picker and the Grid toggle share one 44 dp row, so the
        // schedule itself gets the screen.
        Row(
            Modifier.fillMaxWidth().height(44.dp).padding(horizontal = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box {
                TextButton(onClick = { pickerOpen = true }, compact = true) {
                    Text(channel?.title ?: "Choose channel", fontSize = 13.sp)
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
            Spacer(Modifier.weight(1f))
            TextButton(onClick = onGrid, compact = true) { Text("Grid", fontSize = 13.sp) }
        }
        val programmes = channel?.let { LiveTvGuideReducer.channel(guide, it.id)?.programmes }.orEmpty()
            .filter { it.end > now - LiveTvGuideReducer.SLOT_SECONDS }
        LazyColumn(Modifier.weight(1f)) {
            if (channel != null && programmes.isEmpty()) {
                item {
                    TextButton(onClick = { if (channel.watchable) onAiring(channel) }, enabled = channel.watchable) {
                        Text("No programme information · Watch live")
                    }
                }
            }
            items(programmes, key = { "${it.start}:${it.end}:${it.title}" }) { programme ->
                val airing = programme.start <= now && now < programme.end
                Row(
                    Modifier.fillMaxWidth().height(44.dp).padding(horizontal = 12.dp)
                        .clickable {
                            // Same rule as the grid: the sheet owns the verbs,
                            // and Watch is the first of them.
                            if (channel != null) onFuture(channel, programme)
                        },
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    Text(
                        liveTvTime(programme.start),
                        fontSize = 12.sp,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(
                        programme.title,
                        fontSize = 14.sp,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    if (airing) {
                        Text("NOW", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
                    }
                }
            }
        }
    }
}

@Composable
private fun WideLiveTvBrowser(
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
    guideWindowStart: Long,
    onGuideEarlier: () -> Unit,
    onGuideNow: () -> Unit,
    onGuideLater: () -> Unit,
    canGuideEarlier: Boolean,
    canGuideLater: Boolean,
    focusedChannelId: String?,
    guideFocusTarget: LiveTvGuideFocusTarget?,
    guideAnchorTime: Long?,
    onFocusedChannel: (LiveTvChannel) -> Unit,
    onGuideNavigation: (LiveTvGuideFocusTarget, Long?) -> Unit,
    onReload: () -> Unit,
    onClearFilters: () -> Unit,
    onFullscreen: () -> Unit,
    onFullscreenGuide: () -> Unit,
    onDetail: (LiveTvChannel, LiveTvProgramme) -> Unit,
    onLeave: () -> Unit,
    dvr: DvrScreenState,
    recordingsOpen: Boolean,
    onRecordings: (Boolean) -> Unit,
    dvrChip: DvrChip,
    onDvrChip: (DvrChip) -> Unit,
    onOpenItem: (Long) -> Unit,
    onOpenRecording: (String) -> Unit,
    onMoreLibrary: () -> Unit,
    onMoreAttention: () -> Unit,
    onMoreUpcoming: () -> Unit,
    onOpenRecordingActivity: () -> Unit,
    onRecord: (LiveTvChannel, LiveTvProgramme) -> Unit,
    onRecordSeries: (LiveTvChannel, LiveTvProgramme) -> Unit,
    onRemind: (LiveTvChannel, LiveTvProgramme) -> Unit,
    onStopRecording: (String) -> Unit,
    onForgetReminder: (String) -> Unit,
    onRestore: (String) -> Unit,
    onRuleEnabled: (String, Boolean) -> Unit,
    onRuleNewOnly: (String, Boolean) -> Unit,
    onRuleDelete: (String) -> Unit,
    onRuleMove: (String, Int) -> Unit,
    onManual: (String, Long, Long, String) -> Unit,
) {
    var layoutOpen by remember { mutableStateOf(false) }
    var moreOpen by remember { mutableStateOf(false) }
    val guideFocus = remember { FocusRequester() }
    LaunchedEffect(channels, focusedChannelId) {
        if (channels.none { it.id == focusedChannelId }) {
            val oldIndex = state.channels.indexOfFirst { it.id == focusedChannelId }
            val replacement = if (oldIndex < 0) {
                channels.firstOrNull()
            } else {
                channels.minByOrNull { candidate ->
                    val candidateIndex = state.channels.indexOfFirst { it.id == candidate.id }
                    kotlin.math.abs(candidateIndex - oldIndex)
                }
            }
            replacement?.let(onFocusedChannel)
        }
    }
    val activeFocusId = if (browse == LiveTvBrowseView.Guide) {
        guideFocusTarget?.channelId ?: focusedChannelId
    } else {
        focusedChannelId
    }
    val focused = channels.firstOrNull { it.id == activeFocusId } ?: state.watching ?: channels.firstOrNull()
    val focusedAiring = focused?.let { controller.airing(it, now) } ?: LiveTvAiring()
    val focusedProgramme = if (browse == LiveTvBrowseView.Guide && focused != null) {
        guideFocusTarget?.programmeStart?.let { start ->
            LiveTvGuideReducer.channel(state.guide, focused.id)?.programmes?.firstOrNull { it.start == start }
        }
    } else {
        null
    } ?: focusedAiring.now

    var searchOpen by remember { mutableStateOf(false) }
    val type = LiveTvTypography.current()

    // One 24 dp row. Earlier / Now / Later moved into the grid header, Return
    // to live went away because the picture is a focus target, and the search
    // field became a dialog — three bands of chrome became one.
    // `heightIn`, not `height`: an 11 sp label inside `TvCompactContentPadding`
    // is about 23 dp, so a fixed 24 dp row clips as soon as the television's
    // font scale moves at all. The trailing actions are declared before the
    // status text takes the slack, so a long search term cannot push Layout
    // and More off the row.
    FlowRow(
        Modifier.fillMaxWidth().padding(bottom = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        LiveTvSegment("On now", !recordingsOpen && browse == LiveTvBrowseView.List) {
            onRecordings(false)
            onBrowse(LiveTvBrowseView.List)
        }
        LiveTvSegment(
            "Guide",
            !recordingsOpen && browse == LiveTvBrowseView.Guide,
            Modifier.focusRequester(guideFocus),
        ) {
            onRecordings(false)
            onBrowse(LiveTvBrowseView.Guide)
        }
        LiveTvSegment("Recordings", recordingsOpen) { onRecordings(true) }
        Text(
            buildString {
                val shown = channels.size
                val total = state.channels.size
                append(if (shown == total) "$total channels" else "$shown of $total channels")
                if (state.playing) append(" · 1 tuner in use")
                dvr.indicatorLabel()?.let { append(" · $it") }
                state.guide?.freshness?.takeIf { it != "fresh" }?.let {
                    append(if (it == "stale") " · guide is stale" else " · no guide data")
                }
                state.message.takeIf { it.isNotEmpty() && it != LIVE_TV_STEADY_MESSAGE }
                    ?.let { append(" · $it") }
            },
            style = type.secondary,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.width(180.dp),
        )
        dvr.indicatorLabel()?.let { label ->
            TextButton(onClick = onOpenRecordingActivity, compact = true) {
                Text("● $label", style = type.primary)
            }
        }
        TextButton(onClick = onToggleFavorites, compact = true) {
            Text(if (favoritesOnly) "★ All" else "★ Favorites", style = type.primary)
        }
        TextButton(onClick = { searchOpen = true }, compact = true) {
            Text(
                if (search.isEmpty()) "⌕ Search" else "⌕ ${search.take(12)}",
                style = type.primary,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Box {
            TextButton(onClick = { layoutOpen = true }, compact = true) {
                Text("Layout", style = type.primary)
            }
            DropdownMenu(expanded = layoutOpen, onDismissRequest = { layoutOpen = false }) {
                TvLiveLayout.offered.forEach { choice ->
                    DropdownMenuItem(
                        text = {
                            Text(choice.label + if (choice == layout.presented) " · Selected" else "")
                        },
                        onClick = { layoutOpen = false; onLayout(choice) },
                    )
                }
            }
        }
        Box {
            TextButton(onClick = { moreOpen = true }, compact = true) {
                Text("More", style = type.primary)
            }
            DropdownMenu(expanded = moreOpen, onDismissRequest = { moreOpen = false }) {
                DropdownMenuItem(
                    text = { Text("Refresh channels") },
                    enabled = !state.busy,
                    onClick = { moreOpen = false; onReload() },
                )
                DropdownMenuItem(
                    text = { Text(if (hideProtected) "Show protected" else "Hide protected") },
                    onClick = { moreOpen = false; onToggleProtected() },
                )
                DropdownMenuItem(text = { Text("Stop") }, onClick = { moreOpen = false; controller.stop() })
                DropdownMenuItem(text = { Text("Leave Live TV") }, onClick = { moreOpen = false; onLeave() })
            }
        }
    }

    if (searchOpen) {
        AlertDialog(
            onDismissRequest = { searchOpen = false },
            title = { Text("Find a channel") },
            text = {
                OutlinedTextField(
                    value = search,
                    onValueChange = onSearch,
                    label = { Text("Number, name, or what is on") },
                    modifier = Modifier.fillMaxWidth().testTag("live-tv-channel-search").tvFocusRing(),
                    singleLine = true,
                )
            },
            confirmButton = { TextButton(onClick = { searchOpen = false }) { Text("Done") } },
            dismissButton = {
                TextButton(onClick = { onSearch(""); searchOpen = false }) { Text("Clear") }
            },
        )
    }

    if (recordingsOpen) {
        // Before the empty-lineup screen deliberately: the schedule, the
        // library and the rules are all worth reading on a server whose tuner
        // is unreachable, and an unreachable tuner is exactly when the lineup
        // is empty.
        DvrRecordingsPanel(
            dvr = dvr,
            now = now,
            chip = dvrChip,
            onChip = onDvrChip,
            onOpenItem = onOpenItem,
            onOpenRecording = onOpenRecording,
            onMoreLibrary = onMoreLibrary,
            onMoreAttention = onMoreAttention,
            onMoreUpcoming = onMoreUpcoming,
            onStop = onStopRecording,
            onRestore = onRestore,
            onRuleEnabled = onRuleEnabled,
            onRuleNewOnly = onRuleNewOnly,
            onRuleDelete = onRuleDelete,
            onRuleMove = onRuleMove,
            onForgetReminder = onForgetReminder,
            onManual = onManual,
            modifier = Modifier.fillMaxSize(),
        )
        return
    }

    if (channels.isEmpty()) {
        Box(Modifier.fillMaxSize().background(Color.Black)) {
            if (state.playing) playerSurface()
            Column(
                Modifier.align(Alignment.Center)
                    .background(MaterialTheme.colorScheme.surface.copy(alpha = 0.94f)).padding(20.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Text(
                    state.message.takeIf { it.isNotEmpty() && state.channels.isEmpty() }
                        ?: "No matching channels. Clear search or filters.",
                    style = type.primary,
                )
                TextButton(onClick = onClearFilters, compact = true) { Text("Clear filters") }
            }
        }
        return
    }

    val selectAiring: (LiveTvChannel) -> Unit = { channel ->
        onFocusedChannel(channel)
        if (state.watching?.id == channel.id && state.playing) onFullscreen() else controller.watch(channel)
    }
    val paging = LiveTvGuidePaging(
        canEarlier = canGuideEarlier,
        canLater = canGuideLater,
        onEarlier = onGuideEarlier,
        onNow = onGuideNow,
        onLater = onGuideLater,
    )

    // Lists are tall and narrow; grids are wide.
    if (layout.presented == TvLiveLayout.GuideOverlay) {
        Box(Modifier.fillMaxSize().background(Color.Black)) {
            // The picture draws full-bleed but its focus rect stops at the top
            // of the panel. A focus target whose bounds are the whole screen
            // has no candidate below, left or right of it, so it also always
            // wins Down from the toolbar — which made the panel, its Close and
            // its Favorites unreachable by D-pad.
            LiveTvPicture(
                state,
                playerSurface,
                onFullscreen,
                Modifier.fillMaxSize(),
                focusModifier = Modifier.fillMaxWidth().fillMaxHeight().padding(bottom = 260.dp),
            )
            Column(
                Modifier.fillMaxWidth().height(260.dp).align(Alignment.BottomCenter)
                    .background(MaterialTheme.colorScheme.surface).padding(10.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        listOfNotNull(
                            "Guide",
                            focused?.let { "Focused: ${it.guide_number} ${it.guide_name}" },
                            focusedProgramme?.title,
                            focusedProgramme?.let { liveTvTime(it.start) },
                        ).joinToString(" · "),
                        style = type.primary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Spacer(Modifier.weight(1f))
                    TextButton(onClick = onToggleFavorites, compact = true) {
                        Text(if (favoritesOnly) "★ All" else "★ Favorites", style = type.primary)
                    }
                    TextButton(
                        onClick = { onLayout(TvLiveLayout.GuidePreview) },
                        compact = true,
                    ) { Text("Close", style = type.primary) }
                }
                if (browse == LiveTvBrowseView.Guide) {
                    LiveTvTelevisionGrid(
                        state = state,
                        channels = channels,
                        now = now,
                        guideWindowStart = guideWindowStart,
                        paging = paging,
                        playingChannelId = state.watching?.id,
                        marks = dvr.marks,
                        onAiring = selectAiring,
                        onFuture = onDetail,
                        guideFocusTarget = guideFocusTarget,
                        guideAnchorTime = guideAnchorTime,
                        onGuideNavigation = onGuideNavigation,
                        onFocusedChannel = onFocusedChannel,
                        onToolbarBoundary = { guideFocus.requestFocus() },
                        modifier = Modifier.weight(1f),
                    )
                } else {
                    // The segmented control has to mean something here too:
                    // On now over the picture is the list, not the grid.
                    LiveTvOnNowList(
                        channels = channels,
                        controller = controller,
                        now = now,
                        watchingChannelId = state.watching?.id,
                        focusedChannelId = focusedChannelId,
                        onFocused = onFocusedChannel,
                        onSelect = selectAiring,
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }
    } else if (browse == LiveTvBrowseView.Guide) {
        Column(Modifier.fillMaxSize()) {
            BoxWithConstraints(Modifier.fillMaxWidth()) {
                val previewHeight = minOf(maxWidth * 0.48f * 9f / 16f, maxHeight * 0.45f)
                Row(
                    Modifier.fillMaxWidth().height(previewHeight),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    LiveTvPicture(
                        state, playerSurface, onFullscreen,
                        Modifier.width(previewHeight * 16f / 9f).fillMaxHeight(),
                    )
                    Column(Modifier.weight(1f).verticalScroll(rememberScrollState())) {
                        LiveTvFocusedProgramme(
                            focused, focusedAiring, state.status, focusedProgramme,
                            eyebrow = true, technical = false,
                            actions = {
                                LiveTvFocusedActions(
                                    focused, focusedProgramme, dvr, now,
                                    selectAiring, onRecord, onRecordSeries, onRemind,
                                    onStopRecording, onForgetReminder,
                                )
                            },
                        )
                    }
                }
            }
            LiveTvTelevisionGrid(
                state = state,
                channels = channels,
                now = now,
                guideWindowStart = guideWindowStart,
                paging = paging,
                playingChannelId = state.watching?.id,
                marks = dvr.marks,
                onAiring = selectAiring,
                onFuture = onDetail,
                guideFocusTarget = guideFocusTarget,
                guideAnchorTime = guideAnchorTime,
                onGuideNavigation = onGuideNavigation,
                onFocusedChannel = onFocusedChannel,
                onToolbarBoundary = { guideFocus.requestFocus() },
                modifier = Modifier.weight(1f),
            )
        }
    } else {
        val watching = state.watching ?: focused
        val watchingAiring = watching?.let { controller.airing(it, now) } ?: LiveTvAiring()
        val watchPane: @Composable (Modifier) -> Unit = { paneModifier ->
            Column(
                paneModifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                LiveTvPicture(
                    state, playerSurface, onFullscreen,
                    Modifier.fillMaxWidth().aspectRatio(16f / 9f),
                )
                watching?.let { channel ->
                    dvrRecordingContext(dvr, channel, watchingAiring.now, now)?.let { label ->
                        TextButton(onClick = onOpenRecordingActivity) { Text("● $label") }
                    }
                }
                LiveTvFocusedProgramme(
                    watching, watchingAiring, state.status, watchingAiring.now,
                    actions = {
                        LiveTvFocusedActions(
                            watching, watchingAiring.now, dvr, now,
                            selectAiring, onRecord, onRecordSeries, onRemind,
                            onStopRecording, onForgetReminder,
                        )
                    },
                )
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(onClick = onFullscreen, enabled = state.playing) { Text("Fullscreen") }
                    TextButton(onClick = {
                        if (state.playing) onFullscreenGuide() else onBrowse(LiveTvBrowseView.Guide)
                    }) { Text("Guide") }
                    TextButton(onClick = controller::toggleMute, enabled = state.playing) {
                        Text(if (state.muted) "Unmute" else "Mute")
                    }
                }
                watchingAiring.next?.let { programme ->
                    TextButton(onClick = { watching?.let { onDetail(it, programme) } }) {
                        Text("Up next · ${liveTvTime(programme.start)} · ${programme.title}", style = type.primary)
                    }
                }
            }
        }
        val channelPane: @Composable (Modifier) -> Unit = { paneModifier ->
            LiveTvOnNowList(
                channels = channels,
                controller = controller,
                now = now,
                watchingChannelId = state.watching?.id,
                focusedChannelId = focusedChannelId,
                onFocused = onFocusedChannel,
                onSelect = selectAiring,
                modifier = paneModifier,
            )
        }
        BoxWithConstraints(Modifier.fillMaxSize()) {
            if (maxWidth >= 840.dp || currentFormFactor() == FormFactor.Television) {
                Row(Modifier.fillMaxSize(), horizontalArrangement = Arrangement.spacedBy(20.dp)) {
                    watchPane(Modifier.weight(0.59f).fillMaxHeight())
                    channelPane(Modifier.weight(0.41f).fillMaxHeight())
                }
            } else {
                Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.spacedBy(16.dp)) {
                    watchPane(Modifier.weight(0.5f).fillMaxWidth())
                    channelPane(Modifier.weight(0.5f).fillMaxWidth())
                }
            }
        }
    }
}

/**
 * The picture is a focus target now: Select on it is Fullscreen, which is what
 * let "Return to live" leave the toolbar.
 */
@Composable
private fun LiveTvPicture(
    state: LiveTvPlayerState,
    playerSurface: @Composable () -> Unit,
    onFullscreen: () -> Unit,
    modifier: Modifier = Modifier,
    /// Where the focus target actually sits inside the drawn box. Defaults to
    /// the whole box; the Over picture layout shrinks it above the panel.
    focusModifier: Modifier? = null,
) {
    val type = LiveTvTypography.current()
    Box(modifier.background(Color.Black)) {
        Box(
            (focusModifier ?: Modifier.matchParentSize())
                .tvFocusRing()
                .clickable(enabled = state.playing, onClick = onFullscreen),
        )
        if (state.playing) {
            playerSurface()
            state.watching?.let { watching ->
                Row(
                    Modifier.align(Alignment.TopStart).padding(6.dp),
                    horizontalArrangement = Arrangement.spacedBy(5.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        watching.guide_name,
                        style = type.badge,
                        color = Color.White,
                        modifier = Modifier
                            .background(Color(0x99000000), MaterialTheme.shapes.small)
                            .padding(horizontal = 5.dp, vertical = 2.dp),
                    )
                    Text(watching.title, style = type.secondary, color = Color.White)
                    Text(
                        "LIVE",
                        style = type.eyebrow,
                        color = Color.White,
                        modifier = Modifier
                            .background(Color(0xFFE23A2E), MaterialTheme.shapes.small)
                            .padding(horizontal = 4.dp, vertical = 1.dp),
                    )
                }
            }
            Text(
                "Select · Fullscreen",
                style = type.tertiary,
                color = Color.White.copy(alpha = 0.75f),
                modifier = Modifier.align(Alignment.BottomEnd).padding(6.dp),
            )
        } else {
            Text(
                "Select a channel to watch live",
                style = type.secondary,
                color = Color.White.copy(alpha = 0.7f),
                modifier = Modifier.align(Alignment.Center),
            )
        }
    }
}

/**
 * One muted line for whatever the controller is saying. It replaced a
 * `titleMedium` title plus a full-width status paragraph; it is not optional,
 * because `LiveTvPlayer` puts the tuner failure, the empty-lineup reason and
 * the refused picture-in-picture in exactly this string and nothing else
 * shows them.
 */
@Composable
private fun LiveTvStatusLine(message: String) {
    if (message.isEmpty() || message == LIVE_TV_STEADY_MESSAGE) return
    Text(
        message,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 2,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 2.dp),
    )
}

/** While a channel plays normally there is nothing to say. */
private const val LIVE_TV_STEADY_MESSAGE = "Playing live"

/** The television grid, sized from the width it is actually given. */
@Composable
private fun LiveTvTelevisionGrid(
    state: LiveTvPlayerState,
    channels: List<LiveTvChannel>,
    now: Long,
    guideWindowStart: Long,
    paging: LiveTvGuidePaging,
    playingChannelId: String?,
    marks: DvrGuideMarks,
    onAiring: (LiveTvChannel) -> Unit,
    onFuture: (LiveTvChannel, LiveTvProgramme) -> Unit,
    guideFocusTarget: LiveTvGuideFocusTarget?,
    guideAnchorTime: Long?,
    onGuideNavigation: (LiveTvGuideFocusTarget, Long?) -> Unit,
    onFocusedChannel: (LiveTvChannel) -> Unit,
    onToolbarBoundary: () -> Unit,
    modifier: Modifier = Modifier,
) {
    BoxWithConstraints(modifier) {
        val dimensions = LiveTvGridMetrics.forTelevision(maxWidth)
        val window = remember(guideWindowStart) {
            LiveTvGuideWindow(
                guideWindowStart,
                guideWindowStart +
                    LiveTvGridMetrics.TELEVISION_VISIBLE_SLOTS * LiveTvGuideReducer.SLOT_SECONDS,
            )
        }
        val grid = remember(state.guide, channels, window, now, dimensions) {
            LiveTvGuideReducer.gridLayout(
                guide = state.guide,
                channels = channels,
                window = window,
                now = now,
                pxPerSlot = dimensions.slotWidth.value,
            )
        }
        LiveTvGuideGrid(
            layout = grid,
            slots = remember(window) { LiveTvGuideReducer.gridSlots(window) },
            playingChannelId = playingChannelId,
            dimensions = dimensions,
            marks = marks,
            now = now,
            paging = paging,
            onAiring = onAiring,
            onFuture = onFuture,
            dpadNavigation = true,
            navigationTarget = guideFocusTarget,
            navigationAnchorTime = guideAnchorTime,
            onNavigationState = onGuideNavigation,
            onFocus = { channel, _ -> onFocusedChannel(channel) },
            onToolbarBoundary = onToolbarBoundary,
            modifier = Modifier.fillMaxSize(),
        )
    }
}

@Composable
private fun LiveTvOnNowList(
    channels: List<LiveTvChannel>,
    controller: LiveTvPlayer,
    now: Long,
    watchingChannelId: String?,
    focusedChannelId: String?,
    onFocused: (LiveTvChannel) -> Unit,
    onSelect: (LiveTvChannel) -> Unit,
    modifier: Modifier = Modifier,
) {
    val listState = androidx.compose.foundation.lazy.rememberLazyListState()
    val requesters = remember { mutableMapOf<String, FocusRequester>() }
    var pendingRequester by remember { mutableStateOf<FocusRequester?>(null) }
    LaunchedEffect(channels, focusedChannelId, pendingRequester) {
        val index = channels.indexOfFirst { it.id == focusedChannelId }
        if (index < 0) return@LaunchedEffect
        val requester = requesters[channels[index].id]
        if (requester == null) {
            listState.scrollToItem(index)
        } else {
            requester.requestFocus()
        }
    }
    LazyColumn(modifier, state = listState, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        items(channels, key = { it.id }) { channel ->
            val requester = remember(channel.id) { FocusRequester() }
            DisposableEffect(channel.id, requester) {
                requesters[channel.id] = requester
                pendingRequester = requester
                onDispose {
                    if (requesters[channel.id] === requester) requesters.remove(channel.id)
                    if (pendingRequester === requester) pendingRequester = null
                }
            }
            LiveTvBrowserRow(
                channel = channel,
                airing = controller.airing(channel, now),
                watching = watchingChannelId == channel.id,
                onFocused = { onFocused(channel) },
                onSelect = { onSelect(channel) },
                modifier = Modifier.focusRequester(requester),
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
    modifier: Modifier = Modifier,
) {
    val type = LiveTvTypography.current()
    TextButton(
        onClick = onSelect,
        enabled = channel.watchable,
        compact = true,
        modifier = modifier.fillMaxWidth().heightIn(min = 48.dp)
            .onFocusChanged { if (it.isFocused) onFocused() },
    ) {
        Row(
            Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(7.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                channel.guide_name.take(5),
                style = type.eyebrow,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                modifier = Modifier
                    .size(width = 42.dp, height = 20.dp)
                    .background(MaterialTheme.colorScheme.surfaceVariant, MaterialTheme.shapes.small)
                    .wrapContentHeight(),
            )
            Column(Modifier.weight(1f)) {
                Row(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                    Text(
                        channel.title,
                        style = type.primary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (watching) {
                        Text("● LIVE", style = type.eyebrow, color = MaterialTheme.colorScheme.primary)
                    }
                }
                Text(
                    airing.now?.title
                        ?: if (channel.watchable) "No programme information" else "Protected · not playable",
                    style = type.secondary,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                airing.progress?.let { progress ->
                    LinearProgressIndicator(
                        progress = { progress },
                        modifier = Modifier.fillMaxWidth().height(2.dp),
                    )
                }
            }
            airing.now?.let {
                Text(
                    "until ${liveTvTime(it.end)}",
                    style = type.tertiary,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
            }
        }
    }
}

@Composable
private fun LiveTvFocusedProgramme(
    channel: LiveTvChannel?,
    airing: LiveTvAiring,
    status: LiveTvStatus?,
    selectedProgramme: LiveTvProgramme? = airing.now,
    eyebrow: Boolean = false,
    /// A fixed-height stage measures its children in order, so the signal
    /// meters would push the badges to zero height. They are one Info press
    /// away in fullscreen; the stage does without them.
    technical: Boolean = true,
    /// Record · Record series · Remind me. Emitted above the synopsis for the
    /// same reason `technical` exists: on the fixed stage the last child is
    /// the one that loses its height, and that must never be the buttons.
    actions: @Composable () -> Unit = {},
) {
    val type = LiveTvTypography.current()
    if (eyebrow && channel != null) {
        Text(
            "FOCUSED · ${channel.guide_number} ${channel.guide_name}" +
                (selectedProgramme?.let { " · ${liveTvTime(it.start)}–${liveTvTime(it.end)}" } ?: ""),
            style = type.eyebrow,
            color = MaterialTheme.colorScheme.primary,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
    Text(
        selectedProgramme?.title ?: channel?.guide_name ?: "Select a channel",
        style = type.title,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
    )
    channel?.let {
        if (!eyebrow) {
            Text(
                listOfNotNull(
                    it.title,
                    selectedProgramme?.let { p -> "${liveTvTime(p.start)}–${liveTvTime(p.end)}" },
                ).joinToString(" · "),
                style = type.secondary,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        actions()
        selectedProgramme?.synopsis?.let { synopsis ->
            Text(
                synopsis,
                style = type.secondary,
                maxLines = if (eyebrow) 3 else 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        LiveTvFormatBadges(it)
        if (technical) {
            LiveTvTechnicalDetails(it, status?.takeIf { observed -> observed.channel?.id == null || observed.channel.id == it.id })
        }
    }
}

/**
 * The DVR actions for whatever the ten-foot browser has focused.
 *
 * A guide cell can be focused before a guide has loaded, and the picture stage
 * is drawn whether or not anything is focused at all, so a missing channel or
 * programme is an ordinary state here rather than a caller's mistake.
 */
@Composable
private fun LiveTvFocusedActions(
    channel: LiveTvChannel?,
    programme: LiveTvProgramme?,
    dvr: DvrScreenState,
    now: Long,
    onWatch: (LiveTvChannel) -> Unit,
    onRecord: (LiveTvChannel, LiveTvProgramme) -> Unit,
    onRecordSeries: (LiveTvChannel, LiveTvProgramme) -> Unit,
    onRemind: (LiveTvChannel, LiveTvProgramme) -> Unit,
    onStopRecording: (String) -> Unit,
    onForgetReminder: (String) -> Unit,
) {
    if (channel == null || programme == null) return
    DvrCellActions(
        channel = channel,
        programme = programme,
        dvr = dvr,
        now = now,
        onWatch = { onWatch(channel) },
        onRecord = { onRecord(channel, programme) },
        onRecordSeries = { onRecordSeries(channel, programme) },
        onRemind = { onRemind(channel, programme) },
        onStop = onStopRecording,
        onForgetReminder = onForgetReminder,
    )
}

/**
 * The overlay over the picture: channel and programme, a progress bar, and a
 * strip of neighbouring channels. On a television every press reaches it
 * already decided by the shared contract table.
 */
@Composable
private fun LiveTvOverlay(
    player: androidx.media3.exoplayer.ExoPlayer?,
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
    dvr: DvrScreenState,
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
    guideWindowStart: Long,
    onGuideEarlier: () -> Unit,
    onGuideNow: () -> Unit,
    onGuideLater: () -> Unit,
    canGuideEarlier: Boolean,
    canGuideLater: Boolean,
    guideFocusTarget: LiveTvGuideFocusTarget?,
    guideAnchorTime: Long?,
    onGuideNavigation: (LiveTvGuideFocusTarget, Long?) -> Unit,
    onClosePanel: () -> Unit,
    onStop: () -> Unit,
    onLeave: () -> Unit,
) {
    if (showingInfo) {
        BoxWithConstraints(Modifier.fillMaxSize().padding(20.dp)) {
            channel?.let {
                LiveTvPlaybackInformation(it, status, player, onClosePanel,
                    Modifier.align(Alignment.TopEnd).widthIn(max = 900.dp).fillMaxWidth().heightIn(max = maxHeight))
            }
        }
        return
    }
    val guideFocus = remember { FocusRequester() }
    val type = LiveTvTypography.current()
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
            if (channel != null) {
                dvrRecordingContext(dvr, channel, airing.now, now)?.let {
                    Text("● $it", style = MaterialTheme.typography.labelMedium, color = Color.White)
                }
            }
        }
        Column {
            if (temporaryGuide) {
                // The identical panel the Over picture layout draws, so the
                // grid a viewer meets from fullscreen is the grid they know.
                Column(
                    Modifier.fillMaxWidth().height(260.dp)
                        .background(MaterialTheme.colorScheme.surface).padding(10.dp),
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            listOfNotNull(
                                "Guide",
                                channel?.let { "Focused: ${it.guide_number} ${it.guide_name}" },
                                airing.now?.title,
                            ).joinToString(" · "),
                            style = type.primary,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Spacer(Modifier.weight(1f))
                        TextButton(onClick = onClosePanel, compact = true) {
                            Text("Close", style = type.primary)
                        }
                    }
                    BoxWithConstraints(Modifier.weight(1f)) {
                        val dimensions = LiveTvGridMetrics.forTelevision(maxWidth)
                        val window = remember(guideWindowStart) {
                            LiveTvGuideWindow(
                                guideWindowStart,
                                guideWindowStart +
                                    LiveTvGridMetrics.TELEVISION_VISIBLE_SLOTS *
                                    LiveTvGuideReducer.SLOT_SECONDS,
                            )
                        }
                        val grid = remember(guide, neighbours, window, now, dimensions) {
                            LiveTvGuideReducer.gridLayout(
                                guide = guide,
                                channels = neighbours,
                                window = window,
                                now = now,
                                pxPerSlot = dimensions.slotWidth.value,
                            )
                        }
                        LiveTvGuideGrid(
                            layout = grid,
                            slots = remember(window) { LiveTvGuideReducer.gridSlots(window) },
                            playingChannelId = channel?.id,
                            dimensions = dimensions,
                            marks = dvr.marks,
                            now = now,
                            paging = LiveTvGuidePaging(
                                canEarlier = canGuideEarlier,
                                canLater = canGuideLater,
                                onEarlier = onGuideEarlier,
                                onNow = onGuideNow,
                                onLater = onGuideLater,
                            ),
                            onAiring = onSelect,
                            onFuture = onFuture,
                            dpadNavigation = true,
                            navigationTarget = guideFocusTarget,
                            navigationAnchorTime = guideAnchorTime,
                            onNavigationState = onGuideNavigation,
                            onToolbarBoundary = { guideFocus.requestFocus() },
                            modifier = Modifier.fillMaxSize(),
                        )
                    }
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
                        TvLiveLayout.offered.forEach { choice ->
                            DropdownMenuItem(
                                text = {
                                    Text(
                                        "Layout: ${choice.label}" +
                                            if (choice == layout.presented) " · Selected" else "",
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
    val state by controller.state.collectAsStateWithLifecycle()
    AndroidView(
        factory = { context ->
            PlayerView(context).apply {
                useController = false
                layoutParams = ViewGroup.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT,
                    ViewGroup.LayoutParams.MATCH_PARENT,
                )
                player = controller.player
                keepScreenOn = state.playing
            }
        },
        update = { view ->
            view.player = controller.player
            view.keepScreenOn = state.playing
        },
        modifier = Modifier.fillMaxSize().background(Color.Black),
    )
}

@Composable
private fun LiveTvPlaybackInformation(
    channel: LiveTvChannel,
    status: LiveTvStatus?,
    player: androidx.media3.exoplayer.ExoPlayer?,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var mode by remember { mutableStateOf(PlaybackStatsMode.Standard) }
    var sample by remember(player) { mutableLongStateOf(0L) }
    LaunchedEffect(player) {
        while (true) { sample += 1; delay(1_000) }
    }
    // The sample only exists while the panel is composed; it never polls or
    // renews a tuner lease. A replaced player creates a fresh observation set.
    val facts = remember(player, channel, status, sample) {
        val size = player?.videoSize
        val plan = status?.delivery
        fun seconds(value: Long?) = value?.takeIf { it >= 0 }?.let { String.format(java.util.Locale.US, "%.1f s", it / 1_000.0) } ?: "Not reported"
        val buffered = player?.takeIf { it.currentMediaItem != null }?.let { (it.bufferedPosition - it.currentPosition).coerceAtLeast(0) }
        val edge = player?.takeIf { it.isCurrentMediaItemLive && it.duration != androidx.media3.common.C.TIME_UNSET && it.duration >= 0 }
            ?.let { (it.duration - it.currentPosition).coerceAtLeast(0) }
        val reception = listOfNotNull(
            status?.signal?.strength_percent?.let { "Strength $it%" },
            status?.signal?.quality_percent?.let { "Quality $it%" },
            status?.signal?.symbol_quality_percent?.let { "Symbol $it%" },
        ).joinToString(" · ").ifEmpty { "Not reported" }
        val method = plan?.let {
            when {
                it.video_action == "copy" && it.audio_action == "copy" -> "Remux"
                it.video_action == "copy" -> "Audio converted for this player"
                else -> "Video converted for this player"
            }
        } ?: "Not reported"
        buildList {
            add(PlaybackInfoFact("decode_resolution", "Playing resolution", size?.takeIf { it.width > 0 && it.height > 0 }?.let { "${it.width}×${it.height}" } ?: "Not reported", playbackInfoExplanation("decode_resolution")))
            add(PlaybackInfoFact("source_resolution", "Broadcast source", channel.sourceFormatDescription ?: "Not reported", channel.measuredSource?.observed_at?.let { "Source observed ${liveTvObservedTime(it)}" }))
            add(PlaybackInfoFact("stream_format", "Stream format", plan?.output?.let { "${it.width}×${it.height} · ${it.video_codec.uppercase()}" } ?: "Not reported", "Server delivery metadata; not a player picture measurement."))
            add(PlaybackInfoFact("decode_audio", "Stream audio track", player?.audioFormat?.let { "${it.sampleMimeType ?: "Codec not reported"} · ${it.channelCount.takeIf { count -> count > 0 }?.let { count -> "$count channels" } ?: "Channels not reported"}" } ?: "Not reported", playbackInfoExplanation("decode_audio")))
            add(PlaybackInfoFact("device_audio", "Device audio output", "Not reported"))
            add(PlaybackInfoFact("method", "Delivery method", method, group = "Server work"))
            add(PlaybackInfoFact("player_state", "Playback", player?.let(::playerStateLabel) ?: "Not reported"))
            add(PlaybackInfoFact("subtitles", "Subtitles", player?.let { p -> if (p.currentTracks.groups.any { it.type == androidx.media3.common.C.TRACK_TYPE_TEXT && it.isSelected }) "Selected · rendered by player" else "Off" } ?: "Not reported"))
            add(PlaybackInfoFact("client_loaded", "Buffered on device", seconds(buffered), playbackInfoExplanation("client_loaded"), "Buffer & delivery"))
            add(PlaybackInfoFact("live_edge", "Behind stream live edge", seconds(edge), "Behind latest available media; not broadcast delay.", "Live stream & reception"))
            add(PlaybackInfoFact("reception", "Tuner reception", reception, group = "Live stream & reception"))
            add(PlaybackInfoFact("stalls", "Buffering interruptions", "Not reported", "This live player does not expose an interruption counter.", "Buffer & delivery"))
            add(PlaybackInfoFact("status", "Server state", status?.state ?: "Not reported", playbackInfoExplanation("status"), "Server work"))
            status?.encoder?.let { add(PlaybackInfoFact("encoder", "Encoder", it, group = "Server work")) }
            status?.owner_node_id?.let { add(PlaybackInfoFact("owner", "Server owner", it, group = "Session & history", diagnosticOnly = true)) }
            add(PlaybackInfoFact("sample", "Player sampled", java.text.DateFormat.getTimeInstance().format(java.util.Date()), group = "Session & history"))
        }
    }
    PlaybackInfoPanel(title = channel.title, facts = facts, mode = mode, onMode = { mode = it }, onClose = onClose, modifier = modifier, isLive = true)
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
            val delivery = it.delivery?.let { plan ->
                listOf(
                    if (plan.video_action == "copy") "Original video" else "${plan.output.video_codec.uppercase()} video",
                    if (plan.audio_action == "copy") "Original audio" else "${plan.output.audio_codec.uppercase()} audio",
                    "${plan.output.width}×${plan.output.height}",
                    plan.packaging.uppercase(),
                ).joinToString(" · ")
            } ?: buildList {
                    add("H.264")
                    it.output_height?.let { height -> add("${height}p") }
                    add("AAC")
                    it.encoder?.takeUnless { encoder -> encoder == "pending" }
                        ?.let { encoder -> add("${encoder.uppercase()} encoder") }
                }.joinToString(" · ")
            LiveTvTechnicalRow("Stream format", delivery)
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
    /** Record · Record series · Remind me, beside Watch. */
    actions: @Composable () -> Unit = {},
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
        actions()
        TextButton(onClick = onClose) { Text("Close") }
    }
}
