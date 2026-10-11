@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@file:Suppress("DEPRECATION")

package tv.plurx.app.player

import android.content.Context
import android.media.AudioTrack
import androidx.media3.common.AudioAttributes
import androidx.media3.exoplayer.audio.AudioSink
import androidx.media3.exoplayer.audio.DefaultAudioSink

/** The pinned public compatibility hook exposes the allocated AudioTrack
 * without reflection or a fork. Delegate its unchanged default construction,
 * then inspect the actual state when asynchronous release is reported. */
internal class ContinuousAudioTrackProvider(private val evidence: ContinuousOutputEvidence) : DefaultAudioSink.AudioTrackProvider {
    override fun getAudioTrack(config: AudioSink.AudioTrackConfig, attributes: AudioAttributes, sessionId: Int, context: Context?): AudioTrack {
        val owner = evidence.owner()
        if (owner != null) evidence.audioOutputs.requireCapacity()
        val track = DefaultAudioSink.AudioTrackProvider.DEFAULT.getAudioTrack(config, attributes, sessionId, context)
        if (owner != null) {
            try { evidence.audioOutputs.allocated(track, owner) { track.state == AudioTrack.STATE_UNINITIALIZED } }
            catch (error: Exception) { track.release(); throw error }
            evidence.emit(ContinuousOutputEvidence.Event.AudioOutputOwned, owner)
        }
        return track
    }
    fun released() {
        evidence.audioOutputs.collectReleased().forEach { owner ->
            evidence.emit(ContinuousOutputEvidence.Event.AudioOutputFreed, owner)
        }
    }
}
