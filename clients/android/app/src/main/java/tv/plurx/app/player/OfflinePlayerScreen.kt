@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.ui.PlayerView
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import tv.plurx.app.data.offline.OfflineDownloads
import tv.plurx.app.ui.components.TvButton
import tv.plurx.app.ui.components.formatTime

/** Cache-only playback with the same reducer, timeline, and key adapter as online playback. */
@Composable
fun OfflinePlayerScreen(downloadId: String, onExit: () -> Unit) {
    val records by OfflineDownloads.records.collectAsStateWithLifecycle()
    val record = records.firstOrNull { it.id == downloadId }
    val context = androidx.compose.ui.platform.LocalContext.current
    val player = remember(downloadId) { OfflineDownloads.cacheOnlyPlayer(context) }
    val surfaceFocus = remember { FocusRequester() }
    val focus = remember { PlayerControlFocus() }
    var failure by remember(downloadId) { mutableStateOf<String?>(null) }
    var controlsVisible by remember { mutableStateOf(true) }
    var timelineFocused by remember { mutableStateOf(false) }
    var pendingMs by remember(player) { mutableStateOf<Long?>(null) }
    var positionMs by remember { mutableLongStateOf(record?.positionMs ?: 0L) }
    var durationMs by remember { mutableLongStateOf(record?.durationMs ?: 0L) }
    var isPlaying by remember { mutableStateOf(true) }
    var infoOpen by remember { mutableStateOf(false) }
    var infoMode by remember { mutableStateOf(PlaybackStatsMode.Standard) }
    var repeatCount by remember { mutableIntStateOf(0) }
    var lastInteraction by remember { mutableLongStateOf(0L) }
    var lastFocusedControl by rememberSaveable { mutableStateOf(PlayerControlId.PlayPause) }
    var focusAfterComposition by remember { mutableStateOf<PlayerControlId?>(null) }

    fun poke() {
        controlsVisible = true
        lastInteraction += 1
    }

    fun inputState(): PlayerInputState = when {
        failure != null -> PlayerInputState.Failed
        infoOpen && infoMode != PlaybackStatsMode.Mini -> PlayerInputState.Info
        timelineFocused && pendingMs != null -> PlayerInputState.Scrub
        timelineFocused -> PlayerInputState.Timeline
        !controlsVisible -> PlayerInputState.Hidden
        else -> PlayerInputState.Transport
    }

    fun requestFocus(control: PlayerControlId) {
        focusAfterComposition = control
    }

    fun applyOutcome(outcome: PlayerInputOutcome, input: PlayerContractInput): Boolean {
        fun commit() {
            pendingMs?.let(player::seekTo)
            pendingMs = null
            poke()
        }

        fun focusTransport() {
            val target = lastFocusedControl.takeIf { it.isTransport } ?: PlayerControlId.PlayPause
            controlsVisible = true
            requestFocus(target)
        }

        return when (outcome) {
            PlayerInputOutcome.Reveal -> {
                poke()
                requestFocus(lastFocusedControl)
                true
            }
            PlayerInputOutcome.FocusTransport -> {
                focusTransport()
                true
            }
            PlayerInputOutcome.TogglePlay -> {
                if (player.isPlaying) player.pause() else player.play()
                poke()
                true
            }
            PlayerInputOutcome.Skip -> {
                val delta = if (input == PlayerContractInput.SkipBack || input == PlayerContractInput.Left) -10_000L else 10_000L
                player.seekTo((player.currentPosition + delta).coerceIn(0L, durationMs.coerceAtLeast(0L)))
                poke()
                true
            }
            PlayerInputOutcome.Preview -> {
                val sign = if (input == PlayerContractInput.Left) -1 else 1
                pendingMs = ((pendingMs ?: player.currentPosition) + sign * PlayerInputPolicy.previewStepMs(repeatCount))
                    .coerceIn(0L, durationMs.coerceAtLeast(0L))
                poke()
                true
            }
            PlayerInputOutcome.Commit -> {
                commit()
                true
            }
            PlayerInputOutcome.Cancel -> {
                pendingMs = null
                poke()
                true
            }
            PlayerInputOutcome.CancelThenFocusTransport -> {
                pendingMs = null
                focusTransport()
                poke()
                true
            }
            PlayerInputOutcome.CommitThenTogglePlay -> {
                commit()
                if (player.isPlaying) player.pause() else player.play()
                true
            }
            PlayerInputOutcome.CloseInfo -> {
                infoOpen = false
                poke()
                requestFocus(PlayerControlId.Info)
                true
            }
            PlayerInputOutcome.Hide -> {
                if (infoOpen && infoMode == PlaybackStatsMode.Mini) infoOpen = false
                controlsVisible = false
                true
            }
            PlayerInputOutcome.Exit -> {
                onExit()
                true
            }
            PlayerInputOutcome.ToggleChrome -> {
                if (controlsVisible) controlsVisible = false else {
                    poke()
                    requestFocus(lastFocusedControl)
                }
                true
            }
            PlayerInputOutcome.FocusMarkerOrIgnore -> true
            PlayerInputOutcome.CancelThenFocusMarkerOrIgnore -> {
                pendingMs = null
                poke()
                true
            }
            PlayerInputOutcome.FocusRow,
            PlayerInputOutcome.Activate,
            PlayerInputOutcome.CloseMenu,
            PlayerInputOutcome.MenuFocus,
            PlayerInputOutcome.Ignore,
            -> false
        }
    }

    BackHandler {
        val input = PlayerContractInput.Back
        applyOutcome(PlayerInputPolicy.route(PlayerInputSurface.Touch, inputState(), input), input)
    }

    LaunchedEffect(downloadId) {
        val request = runCatching { OfflineDownloads.completedDownloadRequest(downloadId) }.getOrNull()
        if (request == null) {
            failure = "Download incomplete"
            return@LaunchedEffect
        }
        player.setMediaItem(request.toMediaItem(), record?.positionMs ?: 0)
        player.prepare()
        player.playWhenReady = true
    }

    DisposableEffect(player, record?.id) {
        val listener = object : Player.Listener {
            override fun onPlayerError(error: PlaybackException) {
                failure = "Download incomplete"
            }

            override fun onIsPlayingChanged(playing: Boolean) {
                isPlaying = playing
            }
        }
        player.addListener(listener)
        onDispose {
            val position = player.currentPosition
            val duration = player.duration.takeIf { it > 0 }
            CoroutineScope(Dispatchers.IO).launch {
                OfflineDownloads.recordProgress(downloadId, position, duration)
            }
            player.removeListener(listener)
            player.release()
        }
    }

    LaunchedEffect(player, record?.id) {
        while (true) {
            if (pendingMs == null) positionMs = player.currentPosition.coerceAtLeast(0L)
            durationMs = player.duration.takeIf { it > 0 } ?: durationMs
            delay(250)
        }
    }

    LaunchedEffect(player, record?.id) {
        while (true) {
            delay(10_000)
            OfflineDownloads.recordProgress(
                downloadId,
                player.currentPosition,
                player.duration.takeIf { it > 0 },
            )
        }
    }

    LaunchedEffect(lastInteraction, isPlaying, infoOpen, pendingMs, failure) {
        if (isPlaying && (!infoOpen || infoMode == PlaybackStatsMode.Mini) && pendingMs == null && failure == null) {
            delay(4_000)
            applyOutcome(
                PlayerInputPolicy.route(PlayerInputSurface.TenFoot, inputState(), PlayerContractInput.Idle),
                PlayerContractInput.Idle,
            )
        }
    }

    LaunchedEffect(controlsVisible, infoOpen, focusAfterComposition) {
        if (!controlsVisible && !infoOpen) {
            surfaceFocus.requestFocus()
        } else if (controlsVisible && !infoOpen) {
            focusAfterComposition?.let {
                runCatching { focus.requester(it).requestFocus() }
                focusAfterComposition = null
            }
        }
    }

    Box(
        Modifier
            .fillMaxSize()
            .background(Color.Black)
            .focusRequester(surfaceFocus)
            .focusable()
            .playerInputAdapter(state = ::inputState) { outcome, input, repeats ->
                repeatCount = repeats
                applyOutcome(outcome, input)
            },
    ) {
        AndroidView(
            factory = { PlayerView(it).apply { useController = false; this.player = player } },
            modifier = Modifier.fillMaxSize(),
        )
        if (failure == null) {
            Box(
                Modifier
                    .fillMaxSize()
                    .focusProperties { canFocus = false }
                    .clickable(
                        interactionSource = remember { MutableInteractionSource() },
                        indication = null,
                    ) {
                        val input = PlayerContractInput.TapSurface
                        applyOutcome(PlayerInputPolicy.route(PlayerInputSurface.Touch, inputState(), input), input)
                    },
            )
        }
        if (controlsVisible && (!infoOpen || infoMode == PlaybackStatsMode.Mini) && failure == null) {
            Controls(
                title = record?.title ?: "Downloaded video",
                subtitle = record?.context,
                positionMs = positionMs,
                durationMs = durationMs,
                pendingMs = pendingMs,
                isPlaying = isPlaying,
                requestInitialFocus = true,
                focus = focus,
                lastFocusedControl = lastFocusedControl,
                onControlFocused = { lastFocusedControl = it; lastInteraction += 1 },
                onTimelineFocused = { timelineFocused = it },
                onBack = {
                    val input = PlayerContractInput.Back
                    applyOutcome(PlayerInputPolicy.route(PlayerInputSurface.Touch, inputState(), input), input)
                },
                onPlayPause = { if (player.isPlaying) player.pause() else player.play(); poke() },
                onSeekBack = { player.seekTo((player.currentPosition - 10_000).coerceAtLeast(0)); poke() },
                onSeekForward = { player.seekTo((player.currentPosition + 10_000).coerceAtMost(durationMs)); poke() },
                onScrub = { pendingMs = it },
                onScrubEnd = { pendingMs?.let(player::seekTo); pendingMs = null; poke() },
                onTracks = null,
                onSettings = null,
                onInfo = { infoOpen = true },
                onPip = null,
            )
        }
        if (infoOpen && failure == null) {
            PlaybackInfoOverlay(
                details = PlaybackInfoDetails(
                    title = record?.title ?: "Downloaded video",
                    fileId = record?.fileId ?: 0,
                    delivery = "Downloaded",
                    position = "${formatTime(positionMs)} / ${formatTime(durationMs)}",
                    buffer = String.format(java.util.Locale.US, "%.1f s", (player.bufferedPosition - player.currentPosition).coerceAtLeast(0) / 1_000.0),
                    decodeResolution = player.videoFormat?.takeIf { it.width > 0 && it.height > 0 }
                        ?.let { "${it.width}×${it.height}" },
                    streamRate = player.videoFormat?.bitrate?.takeIf { it > 0 }?.toLong()?.let(::formatBitrate),
                    playerState = if (player.isPlaying) "Playing" else "Paused",
                    transport = "Offline Media3 cache",
                ),
                reasons = emptyList(),
                mode = infoMode,
                onMode = { infoMode = it },
                onDismiss = { infoOpen = false; poke(); requestFocus(PlayerControlId.Info) },
            )
        }
        failure?.let { message ->
            Box(Modifier.fillMaxSize().background(Color.Black), contentAlignment = Alignment.Center) {
                androidx.compose.foundation.layout.Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(message, color = Color.White)
                    TvButton(onClick = onExit, modifier = Modifier.padding(top = 12.dp)) { Text("Back") }
                }
            }
        }
    }
}
