@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.librarychannels

import android.content.Context
import android.os.SystemClock
import androidx.media3.common.MediaItem
import androidx.media3.common.MimeTypes
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.datasource.okhttp.OkHttpDataSource
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import tv.plurx.app.data.Caps
import tv.plurx.app.data.CreateSessionReq
import tv.plurx.app.data.LibraryChannel
import tv.plurx.app.data.LibraryChannelFavourite
import tv.plurx.app.data.LibraryChannelProgramme
import tv.plurx.app.data.LibraryChannelResolved
import tv.plurx.app.data.LibraryChannelSessionRequest
import tv.plurx.app.data.Net
import tv.plurx.app.data.PlurxApi
import java.util.UUID

data class LibraryChannelPlayerState(
    val channels: List<LibraryChannel> = emptyList(),
    val programmes: List<LibraryChannelProgramme> = emptyList(),
    val watching: LibraryChannel? = null,
    val resolved: LibraryChannelResolved? = null,
    val title: String? = null,
    val message: String = "Choose a channel to join its schedule.",
    val busy: Boolean = false,
    val paused: Boolean = false,
)

/**
 * Application-lifetime following controller. Library channels borrow the
 * finite-HLS transport, but never the finite-media progress/marker controller:
 * this is the fence that keeps a scheduled join out of resume and watch state.
 */
class LibraryChannelPlayer private constructor(context: Context) {
    private val appContext = context.applicationContext
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    val player: ExoPlayer = ExoPlayer.Builder(appContext)
        .setMediaSourceFactory(DefaultMediaSourceFactory(OkHttpDataSource.Factory(Net.capabilityClient)))
        .build()
    private val mutableState = MutableStateFlow(LibraryChannelPlayerState())
    val state: StateFlow<LibraryChannelPlayerState> = mutableState.asStateFlow()
    private var api: PlurxApi? = null
    private var profileOrigin: String? = null
    private var playbackId = UUID.randomUUID().toString()
    private var sessionId: String? = null
    private var tuneSequence = 0L
    private var boundary: Job? = null
    private var clockRefresh: Job? = null
    private var serverBaseMs = 0L
    private var monotonicBaseMs = 0L

    init {
        player.addListener(object : Player.Listener {
            override fun onPlaybackStateChanged(playbackState: Int) {
                if (playbackState != Player.STATE_ENDED) return
                val current = mutableState.value
                val channel = current.watching ?: return
                if ((current.resolved?.ends_at_ms ?: 0) > serverNowMs()) {
                    mutableState.value = current.copy(
                        message = "This programme ended early. Waiting for its scheduled boundary.",
                    )
                } else {
                    tune(channel)
                }
            }

            override fun onPlayerError(error: PlaybackException) {
                mutableState.value = mutableState.value.copy(
                    busy = false,
                    message = "This scheduled programme could not continue. Rejoin the channel to try again.",
                )
            }
        })
    }

    fun load(origin: String) {
        if (profileOrigin != origin || api == null) {
            stop(clearProfile = true)
            api = Net.api(origin)
            profileOrigin = origin
            playbackId = UUID.randomUUID().toString()
        }
        refresh()
    }

    fun refresh() {
        val client = api ?: return
        scope.launch {
            try {
                val channels = client.libraryChannels().sortedWith(
                    compareByDescending<LibraryChannel> { it.favourite }.thenBy { it.name.lowercase() },
                )
                val now = System.currentTimeMillis()
                val guide = if (channels.isEmpty()) emptyList() else client.libraryChannelGuide(
                    channels.take(20).joinToString(",") { it.id },
                    now,
                    now + 4 * 60 * 60 * 1_000,
                )
                mutableState.value = mutableState.value.copy(
                    channels = channels,
                    programmes = guide,
                    message = if (mutableState.value.watching != null) mutableState.value.message
                    else if (channels.isEmpty()) "No Library channels yet. An administrator can make the first one."
                    else "${channels.size} scheduled channels · guide times come from the server",
                )
            } catch (_: kotlinx.coroutines.CancellationException) {
            } catch (error: Exception) {
                mutableState.value = mutableState.value.copy(message = error.message ?: "Library channels are unavailable.")
            }
        }
    }

    fun tune(channel: LibraryChannel) {
        if (!channel.enabled) return
        val client = api ?: return
        tuneSequence += 1
        val expected = tuneSequence
        mutableState.value = mutableState.value.copy(busy = true, message = "Joining ${channel.name}…")
        scope.launch {
            try {
                val resolveStarted = SystemClock.elapsedRealtime()
                val resolved = client.resolveLibraryChannel(channel.id)
                recordServerClock(resolved.server_now_ms, resolveStarted)
                if (expected != tuneSequence) return@launch
                val caps = Caps.snapshot(appContext).document
                val started = client.createLibraryChannelSession(
                    channel.id,
                    LibraryChannelSessionRequest(
                        generation_id = resolved.generation_id,
                        occurrence = resolved.occurrence,
                        tune_sequence = expected,
                        playback = CreateSessionReq(
                            playback_id = playbackId,
                            request_id = UUID.randomUUID().toString(),
                            start = resolved.position_ms / 1_000.0,
                            native_subtitles = true,
                            caps = caps,
                        ),
                    ),
                )
                if (expected != tuneSequence || started.library_channel.tune_sequence != expected) {
                    runCatching { client.endHlsSession(started.playback.session_id) }
                    return@launch
                }
                sessionId?.let { old -> runCatching { client.endHlsSession(old) } }
                sessionId = started.playback.session_id
                val title = mutableState.value.programmes.firstOrNull {
                    it.channel_id == channel.id && it.generation_id == resolved.generation_id &&
                        it.cycle == resolved.occurrence.cycle && it.ordinal == resolved.occurrence.ordinal
                }?.title ?: channel.name
                mutableState.value = mutableState.value.copy(
                    watching = channel,
                    resolved = resolved,
                    title = title,
                    busy = false,
                    paused = false,
                    message = "Following live · seeking and watch history are off",
                )
                player.setMediaItem(
                    MediaItem.Builder().setUri(started.playback.playlist_url)
                        .setMimeType(MimeTypes.APPLICATION_M3U8).build(),
                )
                player.prepare()
                player.playWhenReady = true
                scheduleBoundary(channel, resolved, expected)
                scheduleClockRefresh(channel, expected)
                scope.launch {
                    delay(2_000)
                    if (expected != tuneSequence) return@launch
                    val origin = started.playback.media_origin_ms ?: resolved.position_ms
                    val scheduledPosition = (serverNowMs() - resolved.starts_at_ms).coerceAtLeast(0)
                    val playerPosition = origin + player.currentPosition
                    if (scheduledPosition - playerPosition > 2_000) {
                        player.seekTo((scheduledPosition - origin).coerceAtLeast(0))
                    }
                }
            } catch (_: kotlinx.coroutines.CancellationException) {
            } catch (error: Exception) {
                if (expected == tuneSequence) {
                    mutableState.value = mutableState.value.copy(
                        busy = false,
                        message = error.message ?: "The scheduled programme could not start.",
                    )
                }
            }
        }
    }

    fun togglePause() {
        val current = mutableState.value
        val channel = current.watching ?: return
        if (current.paused) {
            if ((current.resolved?.ends_at_ms ?: Long.MAX_VALUE) <= serverNowMs()) {
                tune(channel)
            } else {
                player.play()
                mutableState.value = current.copy(
                    paused = false,
                    message = "Following live · seeking and watch history are off",
                )
            }
        } else {
            player.pause()
            mutableState.value = current.copy(
                paused = true,
                message = "Paused. Resume rejoins server-now if the programme changes.",
            )
        }
    }

    fun setFavourite(channel: LibraryChannel) {
        val client = api ?: return
        scope.launch {
            try {
                client.setLibraryChannelFavourite(channel.id, LibraryChannelFavourite(!channel.favourite))
                refresh()
            } catch (error: Exception) {
                mutableState.value = mutableState.value.copy(message = error.message ?: "Favourite could not be saved.")
            }
        }
    }

    fun stop(clearProfile: Boolean = false) {
        tuneSequence += 1
        boundary?.cancel()
        boundary = null
        clockRefresh?.cancel()
        clockRefresh = null
        player.stop()
        player.clearMediaItems()
        val retiring = sessionId
        sessionId = null
        api?.let { client -> retiring?.let { id -> scope.launch { runCatching { client.endHlsSession(id) } } } }
        mutableState.value = LibraryChannelPlayerState(
            channels = if (clearProfile) emptyList() else mutableState.value.channels,
            programmes = if (clearProfile) emptyList() else mutableState.value.programmes,
        )
        if (clearProfile) {
            api = null
            profileOrigin = null
        }
    }

    private fun scheduleBoundary(channel: LibraryChannel, resolved: LibraryChannelResolved, expected: Long) {
        boundary?.cancel()
        boundary = scope.launch {
            delay((resolved.ends_at_ms - serverNowMs()).coerceAtLeast(0))
            if (expected != tuneSequence) return@launch
            if (mutableState.value.paused) {
                mutableState.value = mutableState.value.copy(
                    message = "The programme changed while paused. Resume to rejoin live.",
                )
            } else tune(channel)
        }
    }

    private fun recordServerClock(serverNowMs: Long, requestStartedMs: Long) {
        val received = SystemClock.elapsedRealtime()
        serverBaseMs = serverNowMs + (received - requestStartedMs).coerceAtLeast(0) / 2
        monotonicBaseMs = received
    }

    private fun serverNowMs(): Long = if (serverBaseMs == 0L) System.currentTimeMillis()
    else serverBaseMs + (SystemClock.elapsedRealtime() - monotonicBaseMs).coerceAtLeast(0)

    private fun scheduleClockRefresh(channel: LibraryChannel, expected: Long) {
        clockRefresh?.cancel()
        val client = api ?: return
        clockRefresh = scope.launch {
            while (expected == tuneSequence) {
                delay(30_000)
                if (expected != tuneSequence || mutableState.value.paused) continue
                val started = SystemClock.elapsedRealtime()
                val fresh = runCatching { client.resolveLibraryChannel(channel.id) }.getOrNull() ?: continue
                recordServerClock(fresh.server_now_ms, started)
                val current = mutableState.value.resolved
                if (current == null || current.generation_id != fresh.generation_id || current.occurrence != fresh.occurrence) {
                    tune(channel)
                    return@launch
                }
                mutableState.value = mutableState.value.copy(resolved = fresh)
                scheduleBoundary(channel, fresh, expected)
            }
        }
    }

    companion object {
        @Volatile private var instance: LibraryChannelPlayer? = null
        fun get(context: Context): LibraryChannelPlayer = instance ?: synchronized(this) {
            instance ?: LibraryChannelPlayer(context).also { instance = it }
        }
    }
}
