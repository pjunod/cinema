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
                // The server owns subtitle selection; a language preference must
                // not re-enable a text rendition it deliberately left disabled.
                .setPreferredTextLanguage(null)
                .setSelectUndeterminedTextLanguage(false)
                .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
                .build()
        }
        val renderers = DefaultRenderersFactory(context).setEnableDecoderFallback(true)
        val player = ExoPlayer.Builder(context)
            .setLoadControl(playbackLoadControl(context, live = role == PlayerRole.LiveTv))
            .setTrackSelector(selector)
            .setRenderersFactory(renderers)
            .setMediaSourceFactory(DefaultMediaSourceFactory(source))
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(C.USAGE_MEDIA)
                    .setContentType(C.AUDIO_CONTENT_TYPE_MOVIE)
                    .build(),
                /* handleAudioFocus = */ true,
            )
            .setHandleAudioBecomingNoisy(true)
            .build()
        if (role == PlayerRole.Audio) player.setWakeMode(C.WAKE_MODE_NETWORK)
        return player
    }
}
