@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.content.Context
import android.content.Intent
import androidx.media3.session.DefaultMediaNotificationProvider
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService

/** Owns the audio plan's media session and foreground notification. */
class PlaybackService : MediaSessionService() {
    override fun onCreate() {
        super.onCreate()
        active = this
        setMediaNotificationProvider(DefaultMediaNotificationProvider(this))
        current?.let(::addSession)
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? = current

    override fun onDestroy() {
        if (active === this) active = null
        super.onDestroy()
    }

    companion object {
        private var current: MediaSession? = null
        private var active: PlaybackService? = null

        /** Called on the app looper before an audio plan starts playback. */
        internal fun attach(context: Context, session: MediaSession) {
            if (current !== session) {
                current?.let { previous -> active?.removeSession(previous) }
                current = session
                active?.addSession(session)
            }
            context.applicationContext.startService(Intent(context, PlaybackService::class.java))
        }

        /** A released plan must retire its notification and service. */
        internal fun detach(context: Context, session: MediaSession) {
            if (current !== session) return
            active?.removeSession(session)
            current = null
            active?.stopSelf()
            context.applicationContext.stopService(Intent(context, PlaybackService::class.java))
        }
    }
}
