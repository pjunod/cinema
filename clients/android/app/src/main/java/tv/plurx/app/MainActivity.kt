package tv.plurx.app

import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.ui.unit.dp
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.NavType
import androidx.navigation.compose.navigation
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import tv.plurx.app.data.Caps
import tv.plurx.app.data.Session
import tv.plurx.app.reminders.ReminderAlarms
import tv.plurx.app.player.PlayerScreen
import tv.plurx.app.player.OfflinePlayerScreen
import tv.plurx.app.player.preplayRouteQuery
import tv.plurx.app.player.preplayTracksFromRoute
import tv.plurx.app.ui.AppViewModel
import tv.plurx.app.ui.ConnectScreen
import tv.plurx.app.ui.DetailScreen
import tv.plurx.app.ui.DownloadsScreen
import tv.plurx.app.ui.HomeScreen
import tv.plurx.app.ui.LibraryScreen
import tv.plurx.app.ui.LoginScreen
import tv.plurx.app.ui.OfflineBookReaderScreen
import tv.plurx.app.ui.Phase
import tv.plurx.app.ui.PhotoScreen
import tv.plurx.app.ui.ReaderScreen
import tv.plurx.app.ui.PdfReaderScreen
import tv.plurx.app.ui.SearchScreen
import tv.plurx.app.ui.SharedLibrariesScreen
import tv.plurx.app.ui.SharedSharingSettingsScreen
import tv.plurx.app.ui.SettingsScreen
import tv.plurx.app.ui.components.LoadingBox
import tv.plurx.app.ui.theme.PlurxTheme
import tv.plurx.app.livetv.LiveTvPlayer
import tv.plurx.app.livetv.LiveTvScreen
import tv.plurx.app.livetv.LiveTvDeveloperScreen
import tv.plurx.app.livetv.LiveTvSettingsScreen
import tv.plurx.app.livetv.DvrApi
import tv.plurx.app.livetv.DvrController
import tv.plurx.app.livetv.DvrRecordingsScreen
import tv.plurx.app.livetv.DvrRecordingDetailScreen
import tv.plurx.app.livetv.DvrCaptureActivityScreen
import tv.plurx.app.librarychannels.LibraryChannelPlayer
import tv.plurx.app.librarychannels.LibraryChannelsScreen
import androidx.compose.ui.platform.LocalContext
import androidx.compose.foundation.layout.Box
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.remember
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.rememberSaveable
import tv.plurx.app.ui.*
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import android.view.KeyEvent
import tv.plurx.app.remote.*

class MainActivity : ComponentActivity() {
    /**
     * The channel a reminder notification's *Watch* asked for. A flow rather
     * than a read of `intent`, because the activity is usually already running
     * when the notification is tapped and `onNewIntent` is the only place that
     * fact arrives.
     */
    private val reminderChannel = MutableStateFlow<String?>(null)
    private val invitationId = MutableStateFlow<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Observe the public Window callback before native dispatch. Keep the
        // original callback, so forwarding cannot re-enter this wrapper.
        val nativeWindowCallback = window.callback
        window.callback = object : android.view.Window.Callback by nativeWindowCallback {
            override fun dispatchKeyEvent(event: KeyEvent): Boolean {
                tv.plurx.app.remote.RemotePhysicalInput.observe(event) { RemoteRuntime.get(applicationContext).physicalInput() }
                return nativeWindowCallback.dispatchKeyEvent(event)
            }
        }
        val rootView = java.lang.ref.WeakReference(window.decorView)
        RemoteRuntime.get(applicationContext).mainWindowEligible = { rootView.get()?.hasWindowFocus() == true }
        // Keep a real-hardware capability snapshot in logcat even before sign
        // in. Decoder/display regressions otherwise surface only as a later
        // server transcode, after the evidence that caused it is gone.
        lifecycleScope.launch { Caps.query(this@MainActivity) }
        reminderChannel.value = intent?.getStringExtra(EXTRA_LIVE_TV_CHANNEL)
        invitationId.value = invitationFrom(intent)
        setContent {
            val vm: AppViewModel = viewModel()
            val preferences by vm.preferences.collectAsStateWithLifecycle()
            PlurxTheme(preferences.theme, preferences.appearance) {
                Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                    AppRoot(vm, reminderChannel, { reminderChannel.value = null }, invitationId, { invitationId.value = null })
                }
            }
        }
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) { super.onWindowFocusChanged(hasFocus); RemoteRuntime.get(applicationContext).windowChanged() }
    override fun onResume() { super.onResume(); RemoteRuntime.scene(true, false) }
    override fun onPause() { RemoteRuntime.scene(false, false); super.onPause() }
    override fun onStop() { if (!isChangingConfigurations) tv.plurx.app.diagnostics.DiagnosticRuntime.background(); if (!isChangingConfigurations) RemoteRuntime.scene(false, true); super.onStop() }
    override fun dispatchTouchEvent(event: android.view.MotionEvent): Boolean {
        if (event.actionMasked == android.view.MotionEvent.ACTION_DOWN) RemoteRuntime.get(applicationContext).physicalInput()
        return super.dispatchTouchEvent(event)
    }
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        reminderChannel.value = intent.getStringExtra(EXTRA_LIVE_TV_CHANNEL)
        invitationId.value = invitationFrom(intent)
    }

    private fun invitationFrom(intent: Intent?): String? = intent?.getStringExtra(tv.plurx.app.invitations.InvitationNotifications.EXTRA_ID)?.takeIf {
        it.length == 43 && runCatching { tv.plurx.app.invitations.InvitationWire.invitation(it) }.isSuccess
    }
    companion object {
        const val EXTRA_LIVE_TV_CHANNEL = "live_tv_channel"
    }
}

@Composable
private fun AppRoot(
    vm: AppViewModel,
    reminderChannel: StateFlow<String?>,
    onReminderChannelUsed: () -> Unit,
    invitationId: StateFlow<String?>,
    onInvitationUsed: () -> Unit,
) {
    val context = LocalContext.current
    val liveTv = LiveTvPlayer.get(context)
    val libraryChannels = LibraryChannelPlayer.get(context)
    val phase by vm.phase.collectAsStateWithLifecycle()
    val busy by vm.busy.collectAsStateWithLifecycle()
    val authError by vm.authError.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()

    LifecycleEventEffect(Lifecycle.Event.ON_START) {
        vm.onForeground()
        // On launch and on every return to the foreground. This is the only
        // moment the phone can learn that a reminder was created, deleted or
        // moved somewhere else while it was closed.
        scope.launch { ReminderAlarms.refresh(context, vm.origin, Session.token) }
    }
    LaunchedEffect(phase) {
        if (phase == Phase.Ready) {
            vm.onForeground()
            ReminderAlarms.refresh(context, vm.origin, Session.token)
        } else {
            liveTv.stop(clearProfile = true)
            libraryChannels.stop(clearProfile = true)
            // A signed-out profile's reminders must not go on firing here —
            // but `Loading` is the phase every cold start passes through, and
            // clearing there would lose every reminder on a launch that could
            // not reach the server.
            if (phase != Phase.Loading) ReminderAlarms.clear(context)
        }
    }

    LaunchedEffect(phase,vm.origin,vm.serverInstanceId,vm.currentUserId) {
        if (phase != Phase.Loading) tv.plurx.app.diagnostics.DiagnosticRuntime.configure(vm.origin,vm.serverInstanceId,vm.currentUserId,tv.plurx.app.player.isTelevision(context))
    }
    Box(Modifier.fillMaxSize()) {
    when (phase) {
        Phase.Loading -> LoadingBox()
        Phase.NeedServer -> ConnectScreen(vm, busy, authError)
        Phase.NeedLogin -> LoginScreen(vm, busy, authError)
        Phase.Ready -> MainNav(vm, reminderChannel, onReminderChannelUsed, invitationId, onInvitationUsed)
    }
        if (phase != Phase.Ready && phase != Phase.NeedLogin) Box(Modifier.align(androidx.compose.ui.Alignment.BottomEnd).navigationBarsPadding().padding(16.dp)) { tv.plurx.app.diagnostics.DiagnosticReportButton(category="connection") }
        tv.plurx.app.diagnostics.DiagnosticReportHost()
    }
}

@Composable
internal fun MainNav(
    vm: AppViewModel,
    reminderChannel: StateFlow<String?>,
    onReminderChannelUsed: () -> Unit,
    invitationId: StateFlow<String?>,
    onInvitationUsed: () -> Unit,
    controller: androidx.navigation.NavHostController? = null,
) {
    val nav = controller ?: rememberNavController()
    val preferences by vm.preferences.collectAsStateWithLifecycle()
    val home by vm.home.collectAsStateWithLifecycle()
    val features by vm.features.collectAsStateWithLifecycle()
    val formFactor = currentFormFactor()
    val meridian = preferences.layout == tv.plurx.app.data.ViewerLayout.Meridian
    var selectedPrimary by rememberSaveable { mutableStateOf("home") }
    var selectedSource by rememberSaveable { mutableStateOf<String?>(null) }
    var sourceNotice by rememberSaveable { mutableStateOf<String?>(null) }
    fun selectPrimary(destination: MeridianDestination) {
        if (selectedPrimary == destination.id) return
        nav.navigate(destination.route) {
            popUpTo(nav.graph.startDestinationId) { saveState = true }
            launchSingleTop = true
            restoreState = true
        }
        selectedPrimary = destination.id
    }
    fun selectSource(ids: String, name: String) {
        if (selectedPrimary == "libraries" && selectedSource == ids && nav.currentDestination?.route != "libraries") return
        val openFromOverview = nav.currentDestination?.route == "libraries"
        if (selectedSource != ids) nav.clearBackStack("libraries")
        nav.navigate("libraries") {
            popUpTo(nav.graph.startDestinationId) { saveState = selectedPrimary != "libraries" }
            launchSingleTop = true
            restoreState = selectedSource == ids
        }
        if (selectedSource != ids || openFromOverview) nav.navigate("library/$ids/${Uri.encode(name)}")
        selectedPrimary = "libraries"
        selectedSource = ids
        sourceNotice = null
    }
    val destinations = meridianDestinations(features, formFactor == FormFactor.Television)
    LaunchedEffect(home.libraries, home.loading, selectedSource) {
        val source = selectedSource ?: return@LaunchedEffect
        if (!home.loading && source.split(',').any { id -> home.libraries.none { it.id.toString() == id } }) {
            nav.clearBackStack("libraries")
            selectedSource = null
            sourceNotice = "The selected library is no longer available. Choose another library."
            if (selectedPrimary == "libraries") {
                nav.navigate("libraries") { popUpTo(nav.graph.startDestinationId); launchSingleTop = true }
            }
        }
    }

    val context = LocalContext.current
    val remote = remember(context) { RemoteRuntime.get(context) }
    val invitations = remember(context) { tv.plurx.app.invitations.InvitationRuntime.get(context) }
    LaunchedEffect(vm.origin, vm.currentUserId, vm.serverInstanceId, invitations.authorizationGeneration) { invitations.configure(vm, remote) }
    LifecycleEventEffect(Lifecycle.Event.ON_START) { invitations.resume() }
    val remoteNavigation = remember { RemoteNavigationCoordinator() }
    val currentEntry by nav.currentBackStackEntryAsState()
    val entryScope = currentEntry?.id ?: "restricted"
    val route = currentEntry?.destination?.route.orEmpty()
    val category = tv.plurx.app.remote.RemoteRoutes.category(route)
    val selectHome by androidx.compose.runtime.rememberUpdatedState({
        if (meridian) selectPrimary(MeridianDestination("home", "Home", "home"))
        else nav.navigate("home") { popUpTo("home") { inclusive = true }; launchSingleTop = true }
    })
    val profileIdentity = Triple(vm.origin, vm.currentUserId, vm.serverInstanceId)
    var previousProfile by remember { mutableStateOf(profileIdentity) }
    LaunchedEffect(profileIdentity) {
        if (previousProfile != profileIdentity) {
            destinations.forEach { nav.clearBackStack(it.route) }
            nav.clearBackStack("libraries")
            nav.navigate("home") { popUpTo(nav.graph.id) { inclusive = true }; launchSingleTop = true }
            selectedPrimary = "home"; selectedSource = null; sourceNotice = null
            remoteNavigation.resetIdentity()
            remoteNavigation.onHome = { selectHome() }
            previousProfile = profileIdentity
        }
    }
    LaunchedEffect(route) {
        destinations.firstOrNull { it.route == route || (it.id == "live-tv" && route.startsWith("live-tv?")) || (it.id == "library-channels" && route.startsWith("library-channels?")) }?.let { selectedPrimary = it.id }
    }
    DisposableEffect(entryScope, category) {
        val entry = currentEntry
        remoteNavigation.enter(entryScope, category) { entry != null && nav.popBackStackFrom(entry) }
        onDispose { remoteNavigation.releaseScope(entryScope) }
    }
    DisposableEffect(remoteNavigation) {
        remoteNavigation.onHome = { selectHome() }
        onDispose { remote.suspendNavigation(); remoteNavigation.resetIdentity() }
    }
    LaunchedEffect(vm.origin, vm.currentUserId, vm.serverInstanceId, remote.enabled, remote.sceneEligible) {
        remote.configure(vm, remoteNavigation, tv.plurx.app.player.isTelevision(context))
        // configure may retire the former authenticated remote identity as well.
        remoteNavigation.onHome = { selectHome() }
    }
    val dvrScope = rememberCoroutineScope()
    val token = Session.token.orEmpty()
    val dvr = androidx.compose.runtime.remember(vm.origin, token, dvrScope) {
        if (token.isEmpty()) null
        else runCatching { DvrController(DvrApi(vm.origin, token), dvrScope) }.getOrNull()
    }
    DisposableEffect(dvr) {
        dvr?.load()
        dvr?.startDuePoll()
        dvr?.startObservation()
        onDispose { dvr?.stopObservation() }
    }
    LifecycleEventEffect(Lifecycle.Event.ON_START) { dvr?.startObservation() }
    LifecycleEventEffect(Lifecycle.Event.ON_STOP) { dvr?.stopObservation() }
    val requestedChannel by reminderChannel.collectAsStateWithLifecycle()
    LaunchedEffect(requestedChannel) {
        val channel = requestedChannel ?: return@LaunchedEffect
        // `launchSingleTop`, because tapping the same notification twice is one
        // request to watch one channel, not two Live TV screens stacked — and
        // two of them would mean two tuner leases.
        nav.navigate("live-tv?channel=${Uri.encode(channel)}") { launchSingleTop = true }
        onReminderChannelUsed()
    }
    Box(Modifier.fillMaxSize().onGloballyPositioned { remoteNavigation.viewport = it.boundsInWindow() }) {
    CompositionLocalProvider(LocalRemoteClient provides remote, LocalRemoteNavigation provides remoteNavigation, LocalRemoteScope provides entryScope) {
    val chromeVisible = meridian && !(route == "shared-libraries" && remoteNavigation.route in setOf("playback", "tracks")) && !route.startsWith("player/") && !route.startsWith("offline/") && !route.startsWith("reader/") && !route.startsWith("pdf-reader/") && !route.startsWith("offline-book/") && !route.startsWith("photo/")
    NavHost(navController = nav, startDestination = "viewer", modifier = Modifier.padding(
        start = if (chromeVisible && formFactor != FormFactor.Compact) (if (formFactor == FormFactor.Television) 88.dp else 208.dp) else 0.dp,
        top = if (chromeVisible) 88.dp else 0.dp,
        bottom = if (chromeVisible && formFactor == FormFactor.Compact) 88.dp else 0.dp,
    )) {
        // A retained graph parent gives every primary root its own saved-state key.
        // Retaining Home itself would alias a popped foreign branch to Home.
        navigation(startDestination = "home", route = "viewer") {
        composable("libraries") { MeridianLibraries(home.libraries, sourceNotice) { selectSource(it.id.toString(), it.name) } }
        composable("home") { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            HomeScreen(
                vm = vm,
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onOpenCollection = { title, libraries ->
                    if (meridian) selectSource(libraries.joinToString(",") { it.id.toString() }, title) else nav.navigate("library/${libraries.joinToString(",") { it.id.toString() }}/${Uri.encode(title)}")
                },
                onSearch = { nav.navigate("search") },
                onPlayFeatured = { id ->
                    val owner = entry.id
                    val authorization = Session.playbackAuthorization()
                    dvrScope.launch {
                        val target = try { vm.homePlayback(id) } catch (cancelled: kotlinx.coroutines.CancellationException) { throw cancelled } catch (_: Exception) { null }
                        if (nav.currentBackStackEntry?.id == owner && Session.playbackAuthorization() == authorization) {
                            if (target != null) nav.navigate("player/${target.itemId}/${target.fileId}/${target.startMs}")
                            else nav.navigate("detail/$id")
                        }
                    }
                },
                onOpenDownloads = { nav.navigate("downloads") },
                onOpenSettings = { nav.navigate("settings") },
                onOpenLiveTv = { nav.navigate("live-tv") },
                onOpenRecordings = { nav.navigate("recordings") },
                onOpenLibraryChannels = { nav.navigate("library-channels") },
                onOpenSharedLibraries = { nav.navigate("shared-libraries") },
            )
            }
        }
        composable(
            "library/{ids}/{name}",
            arguments = listOf(
                navArgument("ids") { type = NavType.StringType },
                navArgument("name") { type = NavType.StringType },
            ),
        ) { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            LibraryScreen(
                vm = vm,
                libraryIds = entry.arguments!!.getString("ids").orEmpty().split(',').mapNotNull(String::toLongOrNull),
                title = entry.arguments!!.getString("name").orEmpty(),
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onBack = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable("search") { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            SearchScreen(
                vm = vm,
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onBack = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable(
            "detail/{id}",
            arguments = listOf(navArgument("id") { type = NavType.LongType }),
        ) { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            DetailScreen(
                vm = vm,
                itemId = entry.arguments!!.getLong("id"),
                onPlay = { itemId, fileId, startMs, tracks ->
                    nav.navigate("player/$itemId/$fileId/$startMs" + preplayRouteQuery(tracks))
                },
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onViewPhoto = { id -> nav.navigate("photo/$id") },
                onRead = { itemId, fileId -> nav.navigate("reader/$itemId/$fileId") },
                onReadPdf = { fileId, expectedSize -> nav.navigate("pdf-reader/$fileId/$expectedSize") },
                onMakeChannel = { item ->
                    nav.navigate("library-channels?seedId=${item.id}&seedKind=${Uri.encode(item.kind)}&seedTitle=${Uri.encode(item.title)}")
                },
                onBack = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable(
            "reader/{itemId}/{fileId}",
            arguments = listOf(
                navArgument("itemId") { type = NavType.LongType },
                navArgument("fileId") { type = NavType.LongType },
            ),
        ) { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            ReaderScreen(
                itemId = entry.arguments!!.getLong("itemId"),
                fileId = entry.arguments!!.getLong("fileId"),
                onExit = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable(
            "pdf-reader/{fileId}/{expectedSize}",
            arguments = listOf(
                navArgument("fileId") { type = NavType.LongType },
                navArgument("expectedSize") { type = NavType.LongType },
            ),
        ) { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            PdfReaderScreen(
                fileId = entry.arguments!!.getLong("fileId"),
                expectedSize = entry.arguments!!.getLong("expectedSize"),
                onExit = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable(
            "photo/{id}",
            arguments = listOf(navArgument("id") { type = NavType.LongType }),
        ) { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            PhotoScreen(
                itemId = entry.arguments!!.getLong("id"),
                onBack = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable("shared-libraries") { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            SharedLibrariesScreen(onBack = { nav.popBackStackFrom(entry) })
            }
        }
        composable("sharing-settings") { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            SharedSharingSettingsScreen(onBack = { nav.popBackStackFrom(entry) },
                onLibraries = { nav.navigate("shared-libraries") }, onDeveloper = { nav.navigate("developer") })
            }
        }
        composable("settings") { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            SettingsScreen(
                vm = vm,
                onBack = { nav.popBackStackFrom(entry) },
                onOpenDeveloper = { nav.navigate("developer") },
                onOpenLiveTvSettings = { nav.navigate("live-tv-settings") },
                onOpenSharing = { nav.navigate("sharing-settings") },
                onOpenRemotes = { nav.navigate("remote-devices") },
            )
            }
        }
        composable(
            "live-tv?channel={channel}",
            arguments = listOf(
                navArgument("channel") {
                    type = NavType.StringType
                    nullable = true
                    defaultValue = null
                },
            ),
        ) { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            LiveTvScreen(
                origin = vm.origin,
                dvrController = dvr,
                initialChannelId = entry.arguments?.getString("channel"),
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onOpenRecording = { id -> nav.navigate("recording/${Uri.encode(id)}") },
                onOpenRecordingActivity = { nav.navigate("recording-activity") },
                onBack = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable("recordings") { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            DvrRecordingsScreen(
                controller = dvr,
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onOpenRecording = { id -> nav.navigate("recording/${Uri.encode(id)}") },
                onOpenActivity = { nav.navigate("recording-activity") },
                onBack = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable(
            "recording/{id}",
            arguments = listOf(navArgument("id") { type = NavType.StringType }),
        ) { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            DvrRecordingDetailScreen(
                controller = dvr,
                recordingId = entry.arguments?.getString("id").orEmpty(),
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onBack = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable("recording-activity") { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            DvrCaptureActivityScreen(
                controller = dvr,
                onOpenRecording = { id -> nav.navigate("recording/${Uri.encode(id)}") },
                onBack = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable(
            "library-channels?seedId={seedId}&seedKind={seedKind}&seedTitle={seedTitle}",
            arguments = listOf(
                navArgument("seedId") { type = NavType.LongType; defaultValue = -1L },
                navArgument("seedKind") { type = NavType.StringType; nullable = true },
                navArgument("seedTitle") { type = NavType.StringType; nullable = true },
            ),
        ) { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            LibraryChannelsScreen(
                vm = vm,
                seedItemId = entry.arguments?.getLong("seedId")?.takeIf { it > 0 },
                seedKind = entry.arguments?.getString("seedKind"),
                seedTitle = entry.arguments?.getString("seedTitle"),
                onWatchFromStart = { itemId, fileId, channelId ->
                    nav.navigate("player/$itemId/$fileId/0?returnChannel=${Uri.encode(channelId)}")
                },
                onBack = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable("remote-devices") { entry -> RemoteDeviceSettings(remote) { nav.popBackStackFrom(entry) } }
        composable("developer") { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            LiveTvDeveloperScreen(origin = vm.origin, onBack = { nav.popBackStackFrom(entry) })
            }
        }
        composable("live-tv-settings") { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            LiveTvSettingsScreen(origin = vm.origin, onBack = { nav.popBackStackFrom(entry) })
            }
        }
        composable("downloads") { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            DownloadsScreen(
                vm = vm,
                onPlay = { id -> nav.navigate("offline/$id") },
                onRead = { id -> nav.navigate("offline-book/$id") },
                onBack = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable(
            "offline-book/{id}",
            arguments = listOf(navArgument("id") { type = NavType.StringType }),
        ) { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            OfflineBookReaderScreen(
                bookId = entry.arguments!!.getString("id").orEmpty(),
                onExit = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable(
            "offline/{id}",
            arguments = listOf(navArgument("id") { type = NavType.StringType }),
        ) { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            OfflinePlayerScreen(
                downloadId = entry.arguments!!.getString("id").orEmpty(),
                onExit = { nav.popBackStackFrom(entry) },
            )
            }
        }
        composable(
            // `audio` and `subtitle` are the viewer's pre-play choice and are
            // optional: an ordinary Play navigates to exactly the route it
            // always did, and the next episode below carries neither — the
            // choice belongs to one playback, not to the queue.
            "player/{itemId}/{fileId}/{startMs}?audio={audio}&subtitle={subtitle}&returnChannel={returnChannel}",
            arguments = listOf(
                navArgument("itemId") { type = NavType.LongType },
                navArgument("fileId") { type = NavType.LongType },
                navArgument("startMs") { type = NavType.LongType },
                navArgument("audio") {
                    type = NavType.StringType
                    nullable = true
                    defaultValue = null
                },
                navArgument("subtitle") {
                    type = NavType.StringType
                    nullable = true
                    defaultValue = null
                },
                navArgument("returnChannel") {
                    type = NavType.StringType
                    nullable = true
                    defaultValue = null
                },
            ),
        ) { entry ->
            CompositionLocalProvider(LocalRemoteScope provides entry.id) {
            val a = entry.arguments!!
            PlayerScreen(
                vm = vm,
                itemId = a.getLong("itemId"),
                fileId = a.getLong("fileId"),
                startMs = a.getLong("startMs"),
                preplayTracks = preplayTracksFromRoute(
                    audio = a.getString("audio"),
                    subtitle = a.getString("subtitle"),
                ),
                returnChannelId = a.getString("returnChannel"),
                onReturnToChannel = {
                    a.getString("returnChannel")?.let(
                        tv.plurx.app.librarychannels.LibraryChannelPlayer::returnToChannel
                    )
                    nav.popBackStack("library-channels", inclusive = false)
                },
                onPlayNext = { target ->
                    nav.navigate("detail/${target.itemId}") { popUpTo("home") }
                    nav.navigate("player/${target.itemId}/${target.fileId}/${target.startMs}")
                },
                onExit = { nav.popBackStackFrom(entry) },
            )
            }
        }
    }
    }
    MeridianChrome(chromeVisible, formFactor, selectedPrimary, destinations, home.libraries,
        onSelect = ::selectPrimary, onLibrary = { selectSource(it.id.toString(), it.name) })
    val requestedInvitation by invitationId.collectAsStateWithLifecycle()
    requestedInvitation?.let { invitation ->
        AlertDialog(onDismissRequest = onInvitationUsed, title = { Text("Open invited screen?") },
            text = { Text("Use the saved Cinema profile that received this invitation. Opening the remote does not take control or start playback. " + invitations.status) },
            confirmButton = { TextButton(onClick = { if (invitations.canOpenTap(invitation)) { invitations.tap(invitation); onInvitationUsed() } }) { Text("Open remote") } },
            dismissButton = { TextButton(onClick = onInvitationUsed) { Text("Dismiss") } })
    }
    RemoteRootOverlay(remote, television = tv.plurx.app.player.isTelevision(context))
    }
    }
}
