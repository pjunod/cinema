@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import androidx.media3.ui.PlayerView
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonPrimitive
import tv.plurx.app.data.Caps
import tv.plurx.app.data.HlsStart
import tv.plurx.app.data.Net
import tv.plurx.app.data.OpticalDecisionRequest
import tv.plurx.app.data.OpticalProgressRequest
import tv.plurx.app.data.OpticalSessionRequest
import tv.plurx.app.data.OpticalTitleDetailDto
import tv.plurx.app.data.OpticalTrackDto
import tv.plurx.app.ui.theme.Accent
import tv.plurx.app.ui.theme.Muted
import java.util.UUID

@Composable
fun OpticalDiscScreen(
    vm: AppViewModel,
    driveId: String,
    onOpenTitle: (drive: String, driveName: String, disc: String, generation: String, title: String) -> Unit,
    onBack: () -> Unit,
) {
    var content by remember(driveId) { mutableStateOf<tv.plurx.app.data.OpticalDriveDiscDto?>(null) }
    var error by remember(driveId) { mutableStateOf<String?>(null) }
    BackHandler(onBack = onBack)
    LaunchedEffect(driveId) {
        runCatching { vm.api().opticalDrive(driveId) }
            .onSuccess { content = it }
            .onFailure { error = opticalFailure(it) }
    }
    OpticalScaffold(title = "Disc", onBack = onBack) {
        when {
            content != null -> {
                val loaded = content!!
                val disc = loaded.drive.disc
                Text(disc?.title ?: "Inserted disc", style = MaterialTheme.typography.headlineLarge)
                Text("${disc?.format?.uppercase() ?: "DISC"} · ${loaded.drive.name}", color = Muted)
                Spacer(Modifier.height(18.dp))
                loaded.titles.forEach { title ->
                    Row(
                        Modifier.fillMaxWidth().clickable {
                            if (disc != null) onOpenTitle(
                                loaded.drive.id,
                                loaded.drive.name,
                                disc.id,
                                disc.media_generation,
                                title.id,
                            )
                        }.padding(vertical = 16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text("Title ${title.id}", style = MaterialTheme.typography.titleMedium)
                            Text(opticalDuration(title.duration_ms), color = Muted)
                        }
                        Text("Browse", color = Accent)
                    }
                }
            }
            error != null -> Text(error!!, color = MaterialTheme.colorScheme.error)
            else -> CircularProgressIndicator(color = Accent)
        }
    }
}

@Composable
fun OpticalTitleScreen(
    vm: AppViewModel,
    driveId: String,
    driveName: String,
    discId: String,
    mediaGeneration: String,
    titleId: String,
    onPlay: (startMs: Long, durationMs: Long?, audio: Int?, subtitle: Int?, title: String) -> Unit,
    onBack: () -> Unit,
) {
    var detail by remember(discId, titleId) { mutableStateOf<OpticalTitleDetailDto?>(null) }
    var error by remember(discId, titleId) { mutableStateOf<String?>(null) }
    var audio by remember { mutableStateOf<Int?>(null) }
    var subtitle by remember { mutableStateOf<Int?>(null) }
    BackHandler(onBack = onBack)
    LaunchedEffect(discId, titleId) {
        runCatching { vm.api().opticalTitle(discId, titleId) }
            .onSuccess { detail = it }
            .onFailure { error = opticalFailure(it) }
    }
    OpticalScaffold(title = "Disc title", onBack = onBack) {
        when {
            detail != null -> {
                val loaded = detail!!
                Text(loaded.disc.title, style = MaterialTheme.typography.headlineLarge)
                Text("Title $titleId · ${opticalDuration(loaded.title.duration_ms)} · $driveName", color = Muted)
                Spacer(Modifier.height(16.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    val resume = loaded.progress?.position_ms?.takeIf { it > 0 } ?: 0
                    Button(onClick = {
                        onPlay(resume, loaded.title.duration_ms, audio, subtitle, loaded.disc.title)
                    }) { Text(if (resume > 0) "Resume" else "Play") }
                    if (resume > 0) {
                        OutlinedButton(onClick = {
                            onPlay(0, loaded.title.duration_ms, audio, subtitle, loaded.disc.title)
                        }) { Text("Play from start") }
                    }
                }
                OpticalTrackChoices("Audio", loaded.title.facts.audio_streams, audio) { audio = it }
                OpticalTrackChoices("Subtitles", loaded.title.facts.subtitle_streams, subtitle, allowOff = true) { subtitle = it }
                if (loaded.chapters.isNotEmpty()) {
                    Text("Chapters", style = MaterialTheme.typography.headlineSmall, modifier = Modifier.padding(top = 20.dp))
                    loaded.chapters.forEachIndexed { index, chapter ->
                        OutlinedButton(
                            onClick = {
                                onPlay(
                                    chapter.start_ms ?: 0,
                                    loaded.title.duration_ms,
                                    audio,
                                    subtitle,
                                    loaded.disc.title,
                                )
                            },
                            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                        ) { Text("Chapter ${chapter.index ?: index + 1} · ${opticalDuration(chapter.start_ms)}") }
                    }
                }
            }
            error != null -> Text(error!!, color = MaterialTheme.colorScheme.error)
            else -> CircularProgressIndicator(color = Accent)
        }
    }
}

@Composable
private fun OpticalTrackChoices(
    label: String,
    tracks: List<OpticalTrackDto>,
    selected: Int?,
    allowOff: Boolean = false,
    onSelect: (Int?) -> Unit,
) {
    if (tracks.isEmpty()) return
    Text(label, style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(top = 20.dp))
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        if (allowOff) {
            OutlinedButton(onClick = { onSelect(null) }, modifier = Modifier.fillMaxWidth()) {
                Text(if (selected == null) "✓ Off" else "Off")
            }
        }
        tracks.forEachIndexed { index, track ->
            val streamIndex = track.index.takeIf { it >= 0 } ?: index
            OutlinedButton(onClick = { onSelect(streamIndex) }, modifier = Modifier.fillMaxWidth()) {
                Text((if (selected == streamIndex) "✓ " else "") + opticalTrackLabel(track))
            }
        }
    }
}

@Composable
fun OpticalPlayerScreen(
    vm: AppViewModel,
    driveId: String,
    discId: String,
    mediaGeneration: String,
    titleId: String,
    title: String,
    startMs: Long,
    durationMs: Long?,
    audio: Int?,
    subtitle: Int?,
    onExit: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val player = remember {
        ExoPlayer.Builder(context)
            .setMediaSourceFactory(DefaultMediaSourceFactory(Net.dataSourceFactory()))
            .build()
    }
    var session by remember { mutableStateOf<HlsStart?>(null) }
    var error by remember { mutableStateOf<String?>(null) }

    suspend fun report(positionMs: Long = player.currentPosition.coerceAtLeast(0)) {
        val active = session ?: return
        runCatching {
            vm.api().opticalProgress(
                discId,
                titleId,
                OpticalProgressRequest(
                    drive_id = driveId,
                    media_generation = mediaGeneration,
                    session_id = active.session_id,
                    position_ms = positionMs,
                    duration_ms = durationMs,
                    audio = audio?.let(::JsonPrimitive),
                    subtitle = subtitle?.let(::JsonPrimitive),
                    recorded_at_ms = System.currentTimeMillis(),
                ),
            )
        }
    }

    BackHandler(onBack = onExit)
    DisposableEffect(player) {
        val listener = object : Player.Listener {
            override fun onPlayerError(failure: PlaybackException) {
                error = failure.localizedMessage ?: "The optical stream ended."
            }
        }
        player.addListener(listener)
        onDispose {
            val active = session
            val finalPosition = player.currentPosition.coerceAtLeast(0)
            scope.launch {
                report(finalPosition)
                active?.let { runCatching { vm.api().endHlsSession(it.session_id) } }
            }
            player.removeListener(listener)
            player.release()
        }
    }
    LaunchedEffect(driveId, discId, titleId, startMs, audio, subtitle) {
        runCatching {
            val caps = Caps.snapshot(context).document
            vm.api().opticalDecision(
                driveId,
                titleId,
                OpticalDecisionRequest(
                    expected_disc_id = discId,
                    media_generation = mediaGeneration,
                    caps = caps,
                    audio = audio,
                    subtitle = subtitle,
                ),
            )
            vm.api().createOpticalSession(
                driveId,
                titleId,
                OpticalSessionRequest(
                    expected_disc_id = discId,
                    media_generation = mediaGeneration,
                    playback_id = UUID.randomUUID().toString(),
                    request_id = UUID.randomUUID().toString(),
                    start = startMs / 1000.0,
                    audio = audio,
                    subtitle_burn = subtitle,
                    caps = caps,
                ),
            )
        }.onSuccess { started ->
            session = started
            player.setMediaItem(MediaItem.fromUri(vm.origin + started.playlist_url))
            player.prepare()
            if (startMs > 0) player.seekTo(startMs)
            player.playWhenReady = true
        }.onFailure { error = opticalFailure(it) }
    }
    LaunchedEffect(session?.session_id) {
        while (isActive && session != null) {
            delay(5_000)
            report()
        }
    }

    Box(Modifier.fillMaxSize().background(androidx.compose.ui.graphics.Color.Black)) {
        when {
            error != null -> Column(Modifier.align(Alignment.Center).padding(24.dp)) {
                Text("Playback unavailable", color = androidx.compose.ui.graphics.Color.White, style = MaterialTheme.typography.headlineSmall)
                Text(error!!, color = androidx.compose.ui.graphics.Color.White.copy(alpha = .72f))
                OutlinedButton(onClick = onExit, modifier = Modifier.padding(top = 16.dp)) { Text("Back") }
            }
            session == null -> CircularProgressIndicator(Modifier.align(Alignment.Center), color = Accent)
            else -> AndroidView(
                factory = { PlayerView(it).apply { this.player = player; useController = true } },
                modifier = Modifier.fillMaxSize(),
            )
        }
        Text(title, color = androidx.compose.ui.graphics.Color.White, modifier = Modifier.align(Alignment.TopStart).padding(18.dp))
    }
}

@Composable
private fun OpticalScaffold(
    title: String,
    onBack: () -> Unit,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)) {
        Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedButton(onClick = onBack) { Text("Back") }
            Spacer(Modifier.width(14.dp))
            Text(title, style = MaterialTheme.typography.titleLarge)
        }
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
            content = content,
        )
    }
}

private fun opticalDuration(milliseconds: Long?): String {
    if (milliseconds == null || milliseconds <= 0) return "Duration unavailable"
    val minutes = milliseconds / 60_000
    return if (minutes >= 60) "${minutes / 60}h ${minutes % 60}m" else "${minutes}m"
}

private fun opticalTrackLabel(track: OpticalTrackDto): String = listOfNotNull(
    track.language,
    track.title,
    track.channels?.let { "$it ch" },
    track.codec.uppercase(),
).joinToString(" · ")

private fun opticalFailure(error: Throwable): String = when {
    error.message?.contains("optical_media_changed") == true -> "The disc changed. Choose the title again."
    error.message?.contains("optical_drive_busy") == true -> "This drive is already in use."
    error.message?.contains("optical_owner_unavailable") == true -> "The drive host is offline."
    else -> error.localizedMessage ?: "The optical source is unavailable."
}
