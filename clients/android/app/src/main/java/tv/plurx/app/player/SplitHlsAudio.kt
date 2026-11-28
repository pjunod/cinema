package tv.plurx.app.player

import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.TrackGroup
import androidx.media3.common.Tracks
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.hls.HlsTrackMetadataEntry
import androidx.media3.exoplayer.hls.playlist.HlsMultivariantPlaylist

@UnstableApi
internal object SplitHlsAudio {
    private val audioPath = Regex("/audio/[0-9a-f]{64}/index\\.m3u8$")
    private val videoPath = Regex("/video/[0-9a-f]{64}/index\\.m3u8$")
    private val stableId = Regex("a(0|[1-9][0-9]*)")

    data class Option(
        val sourceIndex: Long,
        val group: TrackGroup,
        val index: Int,
        val selected: Boolean,
        val default: Boolean,
    )

    fun options(
        master: HlsMultivariantPlaylist,
        tracks: Tracks,
        owns: (String) -> Boolean,
    ): List<Option> {
        if (master.muxedAudioFormat != null || master.variants.isEmpty() ||
            master.variants.any { !owns(it.url.toString()) || !videoPath.containsMatchIn(it.url.path.orEmpty()) }
        ) return emptyList()
        val groupId = master.variants.map { it.audioGroupId }.toSet().singleOrNull()
            ?: return emptyList()
        val renditions = master.audios.filter { it.groupId == groupId }
        if (renditions.any {
                val uri = it.url
                uri == null || !owns(uri.toString()) || !audioPath.containsMatchIn(uri.path.orEmpty())
            } || renditions.map { it.stableRenditionId }.distinct().size != renditions.size ||
            renditions.map { it.name }.distinct().size != renditions.size
        ) return emptyList()

        return renditions.mapNotNull { rendition ->
            val id = rendition.stableRenditionId ?: return@mapNotNull null
            val source = stableId.matchEntire(id)?.groupValues?.get(1)?.toLongOrNull()
                ?: return@mapNotNull null
            tracks.groups.filter { it.type == C.TRACK_TYPE_AUDIO }.flatMap { group ->
                (0 until group.length).mapNotNull { index ->
                    if (!group.isTrackSupported(index) ||
                        !matches(group.getTrackFormat(index), groupId, rendition.name)
                    ) null else Option(source, group.mediaTrackGroup, index, group.isTrackSelected(index),
                        rendition.format.selectionFlags and C.SELECTION_FLAG_DEFAULT != 0)
                }
            }.singleOrNull()
        }
    }

    private fun matches(format: Format, group: String, name: String): Boolean {
        val metadata = format.metadata ?: return false
        return (0 until metadata.length()).any {
            val entry = metadata.get(it) as? HlsTrackMetadataEntry
            entry?.groupId == group && entry.name == name
        }
    }
}
