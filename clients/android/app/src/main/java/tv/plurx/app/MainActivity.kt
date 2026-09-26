package tv.plurx.app

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
import tv.plurx.app.ui.SettingsScreen
import tv.plurx.app.ui.components.LoadingBox
import tv.plurx.app.ui.theme.PlurxTheme
import tv.plurx.app.livetv.LiveTvPlayer
import tv.plurx.app.livetv.LiveTvScreen
import tv.plurx.app.livetv.LiveTvDeveloperScreen
import tv.plurx.app.livetv.DvrApi
import tv.plurx.app.livetv.DvrController
import tv.plurx.app.livetv.DvrRecordingsScreen
import tv.plurx.app.livetv.DvrRecordingDetailScreen
import tv.plurx.app.livetv.DvrCaptureActivityScreen
import tv.plurx.app.librarychannels.LibraryChannelPlayer
import tv.plurx.app.librarychannels.LibraryChannelsScreen
import androidx.compose.ui.platform.LocalContext

class MainActivity : ComponentActivity() {
    /**
     * The channel a reminder notification's *Watch* asked for. A flow rather
     * than a read of `intent`, because the activity is usually already running
     * when the notification is tapped and `onNewIntent` is the only place that
     * fact arrives.
     */
    private val reminderChannel = MutableStateFlow<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Keep a real-hardware capability snapshot in logcat even before sign
        // in. Decoder/display regressions otherwise surface only as a later
        // server transcode, after the evidence that caused it is gone.
        lifecycleScope.launch { Caps.query(this@MainActivity) }
        reminderChannel.value = intent?.getStringExtra(EXTRA_LIVE_TV_CHANNEL)
        setContent {
            val vm: AppViewModel = viewModel()
            val preferences by vm.preferences.collectAsStateWithLifecycle()
            PlurxTheme(preferences.theme, preferences.appearance) {
                Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                    AppRoot(vm, reminderChannel) { reminderChannel.value = null }
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        reminderChannel.value = intent.getStringExtra(EXTRA_LIVE_TV_CHANNEL)
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

    when (phase) {
        Phase.Loading -> LoadingBox()
        Phase.NeedServer -> ConnectScreen(vm, busy, authError)
        Phase.NeedLogin -> LoginScreen(vm, busy, authError)
        Phase.Ready -> MainNav(vm, reminderChannel, onReminderChannelUsed)
    }
}

@Composable
private fun MainNav(
    vm: AppViewModel,
    reminderChannel: StateFlow<String?>,
    onReminderChannelUsed: () -> Unit,
) {
    val nav = rememberNavController()
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
    NavHost(navController = nav, startDestination = "home") {
        composable("home") {
            HomeScreen(
                vm = vm,
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onOpenCollection = { title, libraries ->
                    nav.navigate("library/${libraries.joinToString(",") { it.id.toString() }}/${Uri.encode(title)}")
                },
                onSearch = { nav.navigate("search") },
                onOpenDownloads = { nav.navigate("downloads") },
                onOpenSettings = { nav.navigate("settings") },
                onOpenLiveTv = { nav.navigate("live-tv") },
                onOpenRecordings = { nav.navigate("recordings") },
                onOpenLibraryChannels = { nav.navigate("library-channels") },
            )
        }
        composable(
            "library/{ids}/{name}",
            arguments = listOf(
                navArgument("ids") { type = NavType.StringType },
                navArgument("name") { type = NavType.StringType },
            ),
        ) { entry ->
            LibraryScreen(
                vm = vm,
                libraryIds = entry.arguments!!.getString("ids").orEmpty().split(',').mapNotNull(String::toLongOrNull),
                title = entry.arguments!!.getString("name").orEmpty(),
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onBack = { nav.popBackStackFrom(entry) },
            )
        }
        composable("search") { entry ->
            SearchScreen(
                vm = vm,
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onBack = { nav.popBackStackFrom(entry) },
            )
        }
        composable(
            "detail/{id}",
            arguments = listOf(navArgument("id") { type = NavType.LongType }),
        ) { entry ->
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
        composable(
            "reader/{itemId}/{fileId}",
            arguments = listOf(
                navArgument("itemId") { type = NavType.LongType },
                navArgument("fileId") { type = NavType.LongType },
            ),
        ) { entry ->
            ReaderScreen(
                itemId = entry.arguments!!.getLong("itemId"),
                fileId = entry.arguments!!.getLong("fileId"),
                onExit = { nav.popBackStackFrom(entry) },
            )
        }
        composable(
            "pdf-reader/{fileId}/{expectedSize}",
            arguments = listOf(
                navArgument("fileId") { type = NavType.LongType },
                navArgument("expectedSize") { type = NavType.LongType },
            ),
        ) { entry ->
            PdfReaderScreen(
                fileId = entry.arguments!!.getLong("fileId"),
                expectedSize = entry.arguments!!.getLong("expectedSize"),
                onExit = { nav.popBackStackFrom(entry) },
            )
        }
        composable(
            "photo/{id}",
            arguments = listOf(navArgument("id") { type = NavType.LongType }),
        ) { entry ->
            PhotoScreen(
                itemId = entry.arguments!!.getLong("id"),
                onBack = { nav.popBackStackFrom(entry) },
            )
        }
        composable("settings") { entry ->
            SettingsScreen(vm = vm, onBack = { nav.popBackStackFrom(entry) }, onOpenDeveloper = { nav.navigate("developer") })
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
        composable("recordings") { entry ->
            DvrRecordingsScreen(
                controller = dvr,
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onOpenRecording = { id -> nav.navigate("recording/${Uri.encode(id)}") },
                onOpenActivity = { nav.navigate("recording-activity") },
                onBack = { nav.popBackStackFrom(entry) },
            )
        }
        composable(
            "recording/{id}",
            arguments = listOf(navArgument("id") { type = NavType.StringType }),
        ) { entry ->
            DvrRecordingDetailScreen(
                controller = dvr,
                recordingId = entry.arguments?.getString("id").orEmpty(),
                onOpenItem = { id -> nav.navigate("detail/$id") },
                onBack = { nav.popBackStackFrom(entry) },
            )
        }
        composable("recording-activity") { entry ->
            DvrCaptureActivityScreen(
                controller = dvr,
                onOpenRecording = { id -> nav.navigate("recording/${Uri.encode(id)}") },
                onBack = { nav.popBackStackFrom(entry) },
            )
        }
        composable(
            "library-channels?seedId={seedId}&seedKind={seedKind}&seedTitle={seedTitle}",
            arguments = listOf(
                navArgument("seedId") { type = NavType.LongType; defaultValue = -1L },
                navArgument("seedKind") { type = NavType.StringType; nullable = true },
                navArgument("seedTitle") { type = NavType.StringType; nullable = true },
            ),
        ) { entry ->
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
        composable("developer") { entry ->
            LiveTvDeveloperScreen(origin = vm.origin, onBack = { nav.popBackStackFrom(entry) })
        }
        composable("downloads") { entry ->
            DownloadsScreen(
                vm = vm,
                onPlay = { id -> nav.navigate("offline/$id") },
                onRead = { id -> nav.navigate("offline-book/$id") },
                onBack = { nav.popBackStackFrom(entry) },
            )
        }
        composable(
            "offline-book/{id}",
            arguments = listOf(navArgument("id") { type = NavType.StringType }),
        ) { entry ->
            OfflineBookReaderScreen(
                bookId = entry.arguments!!.getString("id").orEmpty(),
                onExit = { nav.popBackStackFrom(entry) },
            )
        }
        composable(
            "offline/{id}",
            arguments = listOf(navArgument("id") { type = NavType.StringType }),
        ) { entry ->
            OfflinePlayerScreen(
                downloadId = entry.arguments!!.getString("id").orEmpty(),
                onExit = { nav.popBackStackFrom(entry) },
            )
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
