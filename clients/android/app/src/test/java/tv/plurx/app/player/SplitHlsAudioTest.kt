package tv.plurx.app.player

import android.net.Uri
import androidx.media3.common.C
import androidx.media3.common.TrackGroup
import androidx.media3.common.Tracks
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.hls.playlist.HlsMultivariantPlaylist
import androidx.media3.exoplayer.hls.playlist.HlsPlaylistParser
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@UnstableApi
@RunWith(RobolectricTestRunner::class)
class SplitHlsAudioTest {
    @Test
    fun sourceOrdinalsFollowStableIdsFromTheActualMedia3Manifest() {
        val root = "https://media.test/api/v1/hls/test/"
        val video = "a".repeat(64)
        val first = "b".repeat(64)
        val second = "c".repeat(64)
        val body = """
            #EXTM3U
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",NAME="English, Original",DEFAULT=YES,STABLE-RENDITION-ID="a2",URI="audio/$first/index.m3u8"
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",NAME="English Commentary",DEFAULT=NO,STABLE-RENDITION-ID="a9",URI="audio/$second/index.m3u8"
            #EXT-X-STREAM-INF:BANDWIDTH=800000,CODECS="avc1.640029,mp4a.40.2",AUDIO="audio"
            video/$video/index.m3u8
        """.trimIndent()
        val manifest = HlsPlaylistParser().parse(Uri.parse(root + "master.m3u8"),
            body.byteInputStream()) as HlsMultivariantPlaylist
        val groups = manifest.audios.reversed().map { audio ->
            val selected = audio.stableRenditionId == "a9"
            Tracks.Group(TrackGroup(audio.name, audio.format), false,
                intArrayOf(C.FORMAT_HANDLED), booleanArrayOf(selected))
        }
        val tracks = Tracks(groups)
        val options = SplitHlsAudio.options(manifest, tracks) { it.startsWith(root) }
        assertEquals(listOf(2L, 9L), options.map { it.sourceIndex })
        assertEquals(9L, options.single { it.selected }.sourceIndex)
        assertEquals(emptyList<SplitHlsAudio.Option>(),
            SplitHlsAudio.options(manifest, tracks) { !it.contains(second) })
    }
}
