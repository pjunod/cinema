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
    /**
     * A second, independent read. It never gates the lineup and never gates a
     * start: a screen that waited on it would be a screen that cannot tune
     * while a guide host is slow.
     */
    val guide: LiveTvGuide? = null,
    val watching: LiveTvChannel? = null,
    val status: LiveTvStatus? = null,
)

/** Dedicated live player: no VOD controller, watch progress, queue or timeline. */
class LiveTvPlayer private constructor(context: Context) {
    private val context = context.applicationContext
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val hints = LiveTvStartHintStore(this.context)
    private var lease: LiveTvLease? = null
    private var api: LiveTvApi? = null
    private var profile: Pair<String, String>? = null
    private var serial = 0L
    private var heartbeat: Job? = null
    private var guideRefresh: Job? = null
    private var channelChange: Job? = null
    /**
     * Set by the screen while, and only while, the activity is genuinely in
     * picture-in-picture — the one case where the video is still on screen and
     * its tuner is still in use, so the ordinary "the screen went away, drop
     * the tuner" rules must consult this first.
     *
     * There is no in-app dock on Android in this effort. Saying there was
     * would tell a reader that a path nobody wrote is covered here.
     */
    @Volatile private var retain = false
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
                    val next = LiveTvApi(origin, token, context)
                    api = next
                    lease = LiveTvLease(next, hints, scope)
                    profile = identity
                }
                val lineup = api!!.lineup()
                if (mine != serial) return@launch
                mutableState.value = LiveTvPlayerState(channels = lineup.channels,
                    message = if (lineup.channels.isEmpty()) "No channels. Check the saved tuner and channel scan in Settings → Developer."
                    else "Select a channel · lineup ${lineup.freshness}")
                startGuideRefresh(origin, token)
                resumeIfRecent(mine)
            } catch (error: Exception) { if (mine == serial) fail(error) }
        }
    }

    /** Refresh lineup and guide without touching the active decoder or lease. */
    fun refresh() {
        val expectedProfile = profile ?: return
        val client = api ?: return
        mutableState.value = mutableState.value.copy(busy = true, message = "Refreshing channels…")
        scope.launch {
            try {
                val lineup = client.lineup()
                if (profile != expectedProfile) return@launch
                val now = System.currentTimeMillis() / 1000
                val from = now - now.mod(LiveTvGuideReducer.SLOT_SECONDS)
                val guide = runCatching { client.guide(from = from, hours = 6) }.getOrNull()
                if (profile != expectedProfile) return@launch
                val latest = mutableState.value
                val channels = lineup.channels.map { channel ->
                    if (latest.watching?.id == channel.id) latest.watching else channel
                }
                mutableState.value = latest.copy(
                    channels = channels,
                    guide = guide ?: latest.guide,
                    busy = false,
                    message = if (lineup.channels.isEmpty()) {
                        "No channels. Check the saved tuner and channel scan in Settings → Developer."
                    } else {
                        "Channels refreshed · lineup ${lineup.freshness}"
                    },
                )
            } catch (error: Exception) {
                if (profile == expectedProfile) {
                    mutableState.value = mutableState.value.copy(busy = false, message = message(error))
                }
            }
        }
    }

    fun watch(channel: LiveTvChannel, compatibilityRetry: Boolean = false) {
        if (!channel.watchable) return
        val api = api ?: return
        val lease = lease ?: return
        val mine = ++serial
        detach()
        mutableState.value = mutableState.value.copy(busy = true, playing = false, title = channel.title,
            watching = channel, status = null, message = "Starting ${channel.title}…")
        scope.launch {
            try {
                val started = lease.start(channel.id).await() ?: return@launch
                if (mine != serial) return@launch
                attach(channel, started, mine, api, lease, compatibilityRetry)
            } catch (error: Exception) {
                if (mine == serial) { fail(error); stopWithMessage(message(error)) }
            }
        }
    }

    /**
     * Everything a live session needs once it exists: the decoder, the error
     * listener, the media item and the heartbeat.
     *
     * A fresh start and a resume both arrive here, which is exactly what makes
     * "rejoin the session the viewer left" the same thing as "tune it" rather
     * than a second, thinner playback path that drifts.
     */
    private fun attach(
        channel: LiveTvChannel,
        started: LiveTvStarted,
        mine: Long,
        api: LiveTvApi,
        lease: LiveTvLease,
        compatibilityRetry: Boolean,
    ) {
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
                    if (mine == serial && code == "codec_unsupported" && !compatibilityRetry) {
                        retryCompatible(channel, api, lease, LiveTvCompatibility(
                            failed_video = true, failed_audio = true, failed_container = true,
                        ))
                    } else if (mine == serial) stopWithMessage(liveTvMessage(code))
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
            .setLiveConfiguration(MediaItem.LiveConfiguration.Builder().setMaxOffsetMs(8_000).build())
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
                        lease.touchHint()
                        api.keepalive(started.session_id)
                        if (mine != serial) break
                        val status = api.status(started.session_id)
                        if (status.state != "active") throw LiveTvFailure("stream_failed")
                        if (mine == serial) {
                            val observed = status.channel
                            val current = mutableState.value
                            mutableState.value = current.copy(
                                status = status,
                                watching = observed?.takeIf { it.id == current.watching?.id }
                                    ?: current.watching,
                                channels = if (observed == null) current.channels else current.channels.map {
                                    if (it.id == observed.id) observed else it
                                },
                            )
                            expireSourceFormats(System.currentTimeMillis() / 1000)
                        }
                    } else if (watchdog.expired) {
                        stopWithMessage("Live TV stopped after the 30-second no-progress budget. Select a channel to resume.")
                    }
                }
            } catch (error: Exception) {
                if (mine == serial && error is LiveTvFailure &&
                    error.code == "source_format_changed" && !compatibilityRetry) {
                    retryCompatible(channel, api, lease, null)
                } else if (mine == serial) stopWithMessage(message(error))
            }
        }
    }

    /**
     * The open-time step of §3.16: a hint from a run that ended unexpectedly is
     * handed straight back to the owner, and a session the viewer never meant
     * to leave comes back through [attach] — the same path a successful start
     * uses — instead of the channel list.
     *
     * It never auto-tunes (guardrail §4.6). If the owner says the session
     * ended, was retired, or is still activating, this returns quietly and the
     * list the lineup already produced is what the viewer sees.
     */
    private suspend fun resumeIfRecent(expected: Long) {
        val client = api ?: return
        val holder = lease ?: return
        if (expected != serial) return
        val resumed = try {
            holder.resumeIfRecent()
        } catch (error: Exception) {
            if (error is kotlinx.coroutines.CancellationException) throw error
            null
        } ?: return
        if (expected != serial) {
            // A profile change or a press overtook the resume. The session is
            // the lease's now, and stopping is what hands it back.
            runCatching { holder.stop().await() }
            return
        }
        val mine = ++serial
        detach()
        val channel = mutableState.value.channels.firstOrNull { it.id == resumed.channel.id }
            ?: resumed.channel
        mutableState.value = mutableState.value.copy(
            busy = true, playing = false, title = channel.title, watching = channel,
            status = null, message = "Rejoining ${channel.title}…",
        )
        try {
            attach(channel, resumed, mine, client, holder, compatibilityRetry = false)
        } catch (error: Exception) {
            if (mine == serial) { fail(error); stopWithMessage(message(error)) }
        }
    }

    private fun retryCompatible(
        channel: LiveTvChannel,
        api: LiveTvApi,
        lease: LiveTvLease,
        compatibility: LiveTvCompatibility?,
    ) {
        val mine = ++serial
        detach()
        mutableState.value = mutableState.value.copy(
            busy = true, playing = false, watching = channel, status = null,
            message = if (compatibility == null)
                "The broadcast changed format. Selecting a fresh route once…"
            else "The original route was rejected. Retrying once with a compatible conversion…",
        )
        scope.launch {
            try {
                lease.stop().await()
                if (mine != serial) return@launch
                if (compatibility != null) api.retryCompatibility(compatibility)
                watch(channel, compatibilityRetry = true)
            } catch (_: Exception) {
                if (mine == serial) {
                    mutableState.value = mutableState.value.copy(
                        busy = false, watching = null,
                        message = "Cleanup is unconfirmed; retry Stop before opening another channel.",
                    )
                }
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
        mutableState.value = mutableState.value.copy(
            busy = false, playing = false, message = message,
            // A released tuner is not "Watching". Carrying `watching` forward
            // left the list row labelled and the grid cell highlighted for a
            // session that no longer exists.
            watching = null,
            status = null,
            channels = if (clearProfile) emptyList() else mutableState.value.channels,
            // And a guide belongs to the profile whose lineup it was matched
            // against: keeping it across a sign-out rendered one account's
            // programme text over the next account's screen.
            guide = if (clearProfile) null else mutableState.value.guide,
        )
        scope.launch {
            try {
                lease?.stop()?.await()
                if (mine == serial && clearProfile) {
                    // The loop captured `api` in a local, so nulling the field
                    // did not stop it: after a sign-out it kept issuing
                    // authenticated guide reads with the previous profile's
                    // bearer token every twenty minutes, for the life of the
                    // process.
                    endGuideRefresh()
                    api = null; profile = null; lease = null
                }
            } catch (_: Exception) {
                if (mine == serial) mutableState.value = mutableState.value.copy(message = "Owner cleanup is unconfirmed. Retry Stop before opening another channel.")
                // Preserve the old immutable profile and capability for retry.
            }
        }
    }
    /** A message for the viewer that is not a failure of the stream. */
    fun report(message: String) {
        mutableState.value = mutableState.value.copy(message = message)
    }

    /**
     * Ends the guide refresh loop. Separate from [detach] because stopping a
     * session does not end the guide — only losing the profile does.
     */
    private fun endGuideRefresh() {
        guideRefresh?.cancel(); guideRefresh = null
    }

    private fun detach() {
        heartbeat?.cancel(); heartbeat = null
        channelChange?.cancel(); channelChange = null
        val output = player
        player = null
        output?.release()
    }
    private fun fail(error: Exception) { mutableState.value = mutableState.value.copy(busy = false, playing = false, message = message(error)) }
    private fun message(error: Exception): String = if (error is LiveTvFailure) error.message.orEmpty() else liveTvMessage("stream_failed")

    /**
     * The one predicate every teardown path asks. It is here rather than in
     * the screen because the screen is exactly the thing that is going away
     * when it matters.
     */
    fun setRetained(retained: Boolean) {
        // Set only by the picture-in-picture listener and by a confirmed
        // `enterPictureInPictureMode`. There is no in-app dock on Android in
        // this effort — the KDoc on `retain` used to claim one, which would
        // have led a reader to believe an untested path was covered here.

        retain = retained
    }

    /** True when a lifecycle event may release the tuner. */
    fun mayRelease(): Boolean = !retain

    /** Stop, unless something is still showing the picture. */
    fun stopUnlessRetained() {
        if (mayRelease()) stop()
    }

    /**
     * A held channel key is one tuner start, not ten. Superseding requests
     * cancel rather than queue, so a channel-surf ends in exactly one start.
     */
    fun requestChannel(channel: LiveTvChannel) {
        channelChange?.cancel()
        channelChange = scope.launch {
            delay(LiveTvInputPolicy.CHANNEL_COALESCE_MS)
            watch(channel)
        }
    }

    /**
     * The guide poll, on the owner's clock rather than a cadence of this
     * client's own: every answer carries `next_refresh_at`, and the next read
     * is scheduled just after the owner intends to have a new answer. It lives
     * on the controller's scope beside the heartbeat rather than in a
     * `LaunchedEffect` on the screen, so leaving the screen — or entering
     * picture-in-picture — cannot forget the guide.
     */
    private fun startGuideRefresh(origin: String, token: String) {
        guideRefresh?.cancel()
        val api = this.api ?: return
        guideRefresh = scope.launch {
            while (true) {
                // A guide that will not load leaves a working screen: rows fall
                // back to number and callsign and nothing else changes.
                // Check before the request, not only before the state write:
                // suppressing the write still left the request itself going out
                // under credentials the viewer has signed out of.
                if (profile != origin to token) return@launch
                val now = System.currentTimeMillis() / 1000
                val from = now - now.mod(LiveTvGuideReducer.SLOT_SECONDS)
                val fetched = runCatching { api.guide(from = from, hours = 6) }.getOrNull()
                if (profile != origin to token) return@launch
                fetched?.let { mutableState.value = mutableState.value.copy(guide = it) }
                delay(
                    LiveTvGuideReducer.nextPollDelaySeconds(
                        fetched, System.currentTimeMillis() / 1000,
                    ) * 1_000L,
                )
            }
        }
    }

    fun airing(channel: LiveTvChannel, now: Long = System.currentTimeMillis() / 1000): LiveTvAiring =
        LiveTvGuideReducer.airing(mutableState.value.guide, channel.id, now)

    /** Drop observations at 20 minutes or the programme that owned them, whichever ends first. */
    fun expireSourceFormats(now: Long) {
        val latest = mutableState.value
        fun fresh(channel: LiveTvChannel): LiveTvChannel {
            val observed = channel.measuredSource?.observed_at ?: return channel
            val programmeEnd = LiveTvGuideReducer.channel(latest.guide, channel.id)?.programmes
                ?.firstOrNull { it.start <= observed && observed < it.end }?.end
            val ttlEnd = if (observed > Long.MAX_VALUE - 20 * 60) Long.MAX_VALUE else observed + 20 * 60
            val expiry = minOf(ttlEnd, programmeEnd ?: Long.MAX_VALUE)
            return if (now < expiry) channel else channel.copy(source_format = null)
        }
        mutableState.value = latest.copy(
            channels = latest.channels.map(::fresh),
            watching = latest.watching?.let(::fresh),
        )
    }

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
