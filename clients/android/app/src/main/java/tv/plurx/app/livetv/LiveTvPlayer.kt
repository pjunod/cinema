@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.livetv

import android.content.Context
import androidx.media3.common.MediaItem
import androidx.media3.common.MimeTypes
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.datasource.okhttp.OkHttpDataSource
import androidx.media3.exoplayer.DefaultLoadControl
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class LiveTvPlayerState(
    val channels: List<LiveTvChannel> = emptyList(),
    val title: String = "Live TV",
    val message: String = "Select a channel to watch live.",
    val busy: Boolean = false,
    val playing: Boolean = false,
    val paused: Boolean = false,
    val muted: Boolean = false,
)

/** Dedicated live player: no VOD controller, watch progress, queue or timeline. */
class LiveTvPlayer private constructor(context: Context) {
    private val context = context.applicationContext
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val barrier = LiveTvStartBarrier(LiveTvFileBarrierStore(this.context))
    private var lease: LiveTvLease? = null
    private var api: LiveTvApi? = null
    private var profile: Pair<String, String>? = null
    private var serial = 0L
    private var heartbeat: Job? = null
    private val mutableState = MutableStateFlow(LiveTvPlayerState())
    val state = mutableState.asStateFlow()
    var player: ExoPlayer? = null
        private set

    fun load(origin: String, token: String) {
        val mine = ++serial
        detach()
        mutableState.value = LiveTvPlayerState(busy = true, message = "Loading channels…")
        scope.launch {
            try {
                lease?.stop()?.await()
                if (mine != serial) return@launch
                val identity = origin to token
                if (profile != identity || api == null) {
                    val next = LiveTvApi(origin, token)
                    api = next
                    lease = LiveTvLease(next, barrier, scope)
                    profile = identity
                }
                val lineup = api!!.lineup()
                if (mine != serial) return@launch
                mutableState.value = LiveTvPlayerState(channels = lineup.channels,
                    message = if (lineup.channels.isEmpty()) "No channels. Check the saved tuner and channel scan in Settings → Developer."
                    else "Select a channel · lineup ${lineup.freshness}")
            } catch (error: Exception) { if (mine == serial) fail(error) }
        }
    }

    fun watch(channel: LiveTvChannel) {
        if (!channel.watchable) return
        val api = api ?: return
        val lease = lease ?: return
        val mine = ++serial
        detach()
        mutableState.value = mutableState.value.copy(busy = true, playing = false, title = channel.title, message = "Starting ${channel.title}…")
        scope.launch {
            try {
                val started = lease.start(channel.id).await() ?: return@launch
                if (mine != serial) return@launch
                val output = ExoPlayer.Builder(context)
                    .setMediaSourceFactory(DefaultMediaSourceFactory(OkHttpDataSource.Factory(api.mediaClient)))
                    .setLoadControl(DefaultLoadControl.Builder().setBufferDurationsMs(4_000, 12_000, 1_000, 2_000).build())
                    .build()
                player = output
                // Both of these tear the player down, and they arrive from
                // inside ExoPlayer's own listener iteration. Releasing a player
                // re-entrantly from its callback is not a documented-safe
                // operation, so the teardown is posted: `Dispatchers.Main`
                // rather than the scope's `Main.immediate`, which would run it
                // inline and change nothing.
                output.addListener(object : Player.Listener {
                    override fun onPlayerError(error: PlaybackException) {
                        if (mine != serial) return
                        val code = liveTvPlaybackErrorCode(error.errorCode)
                        scope.launch(Dispatchers.Main) {
                            if (mine == serial) stopWithMessage(liveTvMessage(code))
                        }
                    }
                    override fun onPlaybackStateChanged(playbackState: Int) {
                        if (mine != serial || playbackState != Player.STATE_ENDED) return
                        scope.launch(Dispatchers.Main) {
                            if (mine == serial) stopWithMessage(liveTvMessage("stream_failed"))
                        }
                    }
                })
                output.setMediaItem(MediaItem.Builder().setUri(api.playlistUrl(started.session_id))
                    .setMimeType(MimeTypes.APPLICATION_M3U8)
                    .setLiveConfiguration(MediaItem.LiveConfiguration.Builder().setTargetOffsetMs(4_000).setMaxOffsetMs(8_000).build())
                    .build())
                output.prepare()
                output.play()
                mutableState.value = mutableState.value.copy(playing = true, busy = false, paused = false, muted = false, message = "Playing live")
                val watchdog = LiveTvWatchdog()
                heartbeat = scope.launch {
                    try {
                        while (mine == serial) {
                            delay(5_000)
                            if (mine != serial) break
                            val counters = output.videoDecoderCounters
                            counters?.ensureUpdated()
                            if (watchdog.observe(counters?.renderedOutputBufferCount ?: 0, output.isPlaying)) {
                                lease.heartbeatMarker()
                                api.keepalive(started.session_id)
                                if (mine != serial) break
                                if (api.status(started.session_id).state != "active") throw LiveTvFailure("stream_failed")
                            } else if (watchdog.expired) {
                                stopWithMessage("Live TV stopped after the 30-second no-progress budget. Select a channel to resume.")
                            }
                        }
                    } catch (error: Exception) {
                        if (mine == serial) stopWithMessage(message(error))
                    }
                }
            } catch (error: Exception) {
                if (mine == serial) { fail(error); stopWithMessage(message(error)) }
            }
        }
    }

    fun togglePause() {
        val output = player ?: return
        val paused = output.playWhenReady
        if (paused) output.pause() else output.play()
        mutableState.value = mutableState.value.copy(paused = paused, message = if (paused)
            "Paused. The tuner is released after the no-progress budget; resuming has no rewind guarantee." else "Playing live")
    }
    fun toggleMute() {
        val output = player ?: return
        val muted = output.volume > 0
        output.volume = if (muted) 0f else 1f
        mutableState.value = mutableState.value.copy(muted = muted)
    }
    fun stop(clearProfile: Boolean = false) {
        stopWithMessage("Live TV stopped. Select a channel to resume.", clearProfile)
    }
    private fun stopWithMessage(message: String, clearProfile: Boolean = false) {
        val mine = ++serial
        detach()
        mutableState.value = mutableState.value.copy(busy = false, playing = false, message = message,
            channels = if (clearProfile) emptyList() else mutableState.value.channels)
        scope.launch {
            try {
                lease?.stop()?.await()
                if (mine == serial && clearProfile) { api = null; profile = null; lease = null }
            } catch (_: Exception) {
                if (mine == serial) mutableState.value = mutableState.value.copy(message = "Owner cleanup is unconfirmed. Retry Stop before opening another channel.")
                // Preserve the old immutable profile and capability for retry.
            }
        }
    }
    private fun detach() {
        heartbeat?.cancel(); heartbeat = null
        val output = player
        player = null
        output?.release()
    }
    private fun fail(error: Exception) { mutableState.value = mutableState.value.copy(busy = false, playing = false, message = message(error)) }
    private fun message(error: Exception): String = if (error is LiveTvFailure) error.message.orEmpty() else liveTvMessage("stream_failed")

    companion object {
        @Volatile private var instance: LiveTvPlayer? = null
        fun get(context: Context): LiveTvPlayer = instance ?: synchronized(this) {
            instance ?: LiveTvPlayer(context).also { instance = it }
        }
    }
}

internal fun liveTvPlaybackErrorCode(code: Int): String = when (code) {
    PlaybackException.ERROR_CODE_DECODER_INIT_FAILED,
    PlaybackException.ERROR_CODE_DECODER_QUERY_FAILED,
    PlaybackException.ERROR_CODE_DECODING_FAILED,
    PlaybackException.ERROR_CODE_DECODING_FORMAT_EXCEEDS_CAPABILITIES,
    PlaybackException.ERROR_CODE_DECODING_FORMAT_UNSUPPORTED -> "codec_unsupported"
    else -> "stream_failed"
}
