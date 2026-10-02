@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.content.Context
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.TransferListener
import androidx.media3.datasource.cache.CacheDataSource
import androidx.media3.datasource.okhttp.OkHttpDataSource
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import androidx.media3.exoplayer.trackselection.DefaultTrackSelector

/** The caller supplies the transport; the role selects only player behavior. */
internal enum class PlayerRole { Finite, Successor, LiveTv, LibraryChannel, Offline, Audio }

/** The attributes every plurx player plays under. */
internal val PLURX_MEDIA_AUDIO_ATTRIBUTES: AudioAttributes = AudioAttributes.Builder()
    .setUsage(C.USAGE_MEDIA)
    .setContentType(C.AUDIO_CONTENT_TYPE_MOVIE)
    .build()

/**
 * Audio focus, and pausing when the output becomes noisy, belong to the one
 * player the viewer hears. A prepared successor plays silently before its
 * switch; if it requested focus, the platform would take focus away from the
 * player on screen and Media3 would pause it — a quality change that freezes
 * the picture it was meant to replace without a gap.
 */
internal fun handlesAudioFocus(role: PlayerRole): Boolean = role != PlayerRole.Successor

/** The one player capability that focus ownership moves. */
internal fun interface AudioFocusOwner {
    fun ownAudioFocus(owns: Boolean)
}

internal fun ExoPlayer.asAudioFocusOwner(): AudioFocusOwner = AudioFocusOwner { owns ->
    setAudioAttributes(PLURX_MEDIA_AUDIO_ATTRIBUTES, owns)
    setHandleAudioBecomingNoisy(owns)
}

/**
 * Move focus from the player going silent to the player becoming audible.
 * The release is issued first, but each player applies it on its own
 * playback thread, so the platform may still see the new request before the
 * old abandon; a late loss on the outgoing player is harmless because it is
 * already paused (commit) or being retired (rollback).
 */
internal fun handOverAudioFocus(from: AudioFocusOwner, to: AudioFocusOwner) {
    from.ownAudioFocus(false)
    to.ownAudioFocus(true)
}

/**
 * One construction path for every player. In particular, Offline still has a
 * cache-only source with no account-bearing upstream, and only finite players
 * request television tunneling. Successor inherits the incumbent's setting.
 */
@UnstableApi
internal class PlurxPlayerBuilder(private val context: Context, private val role: PlayerRole) {
    fun build(
        dataSource: DataSource.Factory,
        audioLanguage: String? = null,
        transferListener: TransferListener? = null,
    ): ExoPlayer {
        require(role != PlayerRole.Offline || dataSource is CacheDataSource.Factory) {
            "Offline playback requires a cache-only data source"
        }
        val source = if (transferListener == null) {
            dataSource
        } else {
            require(dataSource is OkHttpDataSource.Factory) {
                "The progressive transfer listener requires an OkHttp data source"
            }
            dataSource.setTransferListener(transferListener)
        }
        val selector = DefaultTrackSelector(context).apply {
            parameters = buildUponParameters()
                .setPreferredAudioLanguage(audioLanguage)
                .setTunnelingEnabled(
                    role in setOf(PlayerRole.Finite, PlayerRole.Successor) &&
                        isTelevision(this@PlurxPlayerBuilder.context),
                )
                .apply {
                    // Controller-managed playback owns subtitle selection.
                    // Live TV, library channels and offline HLS need Media3 to
                    // discover and render the text renditions in their playlists.
                    if (role in setOf(PlayerRole.Finite, PlayerRole.Successor, PlayerRole.Audio)) {
                        setPreferredTextLanguage(null)
                        setSelectUndeterminedTextLanguage(false)
                        setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
                    }
                }
                .build()
        }
        val renderers = DefaultRenderersFactory(context).setEnableDecoderFallback(true)
        val player = ExoPlayer.Builder(context)
            .setLoadControl(playbackLoadControl(
                context,
                live = role == PlayerRole.LiveTv,
                role = when (role) {
                    PlayerRole.Successor -> BufferRole.Successor
                    PlayerRole.LiveTv -> BufferRole.Live
                    else -> BufferRole.Incumbent
                },
            ))
            .setTrackSelector(selector)
            .setRenderersFactory(renderers)
            .setMediaSourceFactory(DefaultMediaSourceFactory(source))
            .setAudioAttributes(PLURX_MEDIA_AUDIO_ATTRIBUTES, handlesAudioFocus(role))
            .setHandleAudioBecomingNoisy(handlesAudioFocus(role))
            .build()
        if (role == PlayerRole.Audio) player.setWakeMode(C.WAKE_MODE_NETWORK)
        return player
    }
}
