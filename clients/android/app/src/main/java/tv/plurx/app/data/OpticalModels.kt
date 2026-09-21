package tv.plurx.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

@Serializable
data class OpticalDriveStateDto(
    val state: String,
    val media_generation: String? = null,
    val disc_id: String? = null,
    val title_id: String? = null,
    val session_id: String? = null,
    val reason: String? = null,
)

@Serializable
data class OpticalRequirementDto(val id: String, val status: String, val detail: String)

@Serializable
data class OpticalDiscSummaryDto(
    val id: String,
    val media_generation: String,
    val format: String,
    val display_title: String? = null,
    val volume_label: String? = null,
    val suggested_title_id: String? = null,
) {
    val title: String get() = display_title ?: volume_label ?: "Inserted disc"
}

@Serializable
data class OpticalDriveDto(
    val id: String,
    val name: String,
    val owner_node_id: String,
    val enabled: Boolean,
    val state: OpticalDriveStateDto,
    val requirements: List<OpticalRequirementDto> = emptyList(),
    val disc: OpticalDiscSummaryDto? = null,
)

@Serializable
data class OpticalTitleSummaryDto(
    val id: String,
    val duration_ms: Long? = null,
    val angles: Int = 1,
    val matched_item_id: Long? = null,
    val match_kind: String? = null,
)

@Serializable
data class OpticalDriveDiscDto(
    val drive: OpticalDriveDto,
    val titles: List<OpticalTitleSummaryDto> = emptyList(),
)

@Serializable
data class OpticalDiscDto(
    val disc_id: String,
    val format: String,
    val volume_label: String? = null,
    val display_title: String? = null,
) {
    val title: String get() = display_title ?: volume_label ?: "Inserted disc"
}

@Serializable
data class OpticalTrackDto(
    val index: Int = -1,
    val codec: String,
    val channels: Int? = null,
    val language: String? = null,
    val title: String? = null,
    val default: Boolean = false,
    val forced: Boolean = false,
)

@Serializable
data class OpticalFactsDto(
    val video_codec: String? = null,
    val width: Int? = null,
    val height: Int? = null,
    val hdr: String? = null,
    val duration_ms: Long? = null,
    val audio_streams: List<OpticalTrackDto> = emptyList(),
    val subtitle_streams: List<OpticalTrackDto> = emptyList(),
)

@Serializable
data class OpticalTitleDto(
    val disc_id: String,
    val title_id: String,
    val angles: Int = 1,
    val facts: OpticalFactsDto,
    val duration_ms: Long? = null,
    val matched_item_id: Long? = null,
    val match_kind: String? = null,
)

@Serializable
data class OpticalChapterDto(
    val index: Int? = null,
    val start_ms: Long? = null,
    val end_ms: Long? = null,
)

@Serializable
data class OpticalProgressDto(
    val disc_id: String,
    val title_id: String,
    val angle: Int,
    val position_ms: Long,
    val duration_ms: Long? = null,
    val watched: Boolean,
    val updated_at_ms: Long? = null,
)

@Serializable
data class OpticalTitleDetailDto(
    val disc: OpticalDiscDto,
    val title: OpticalTitleDto,
    val chapters: List<OpticalChapterDto> = emptyList(),
    val progress: OpticalProgressDto? = null,
)

@Serializable
data class OpticalDecisionRequest(
    val expected_disc_id: String,
    val media_generation: String,
    val angle: Int = 1,
    val caps: DeviceCaps,
    val force: String? = null,
    val audio: Int? = null,
    val subtitle: Int? = null,
)

@Serializable
data class OpticalDecisionDto(
    val method: String,
    val play_url: String,
    val source: OpticalFactsDto? = null,
    val audio: List<OpticalTrackDto> = emptyList(),
    val subtitles: List<OpticalTrackDto> = emptyList(),
    val ladder: List<Rung> = emptyList(),
    val delivered_dynamic_range: String? = null,
)

@Serializable
data class OpticalSessionRequest(
    val expected_disc_id: String,
    val media_generation: String,
    val angle: Int = 1,
    val playback_id: String,
    val request_id: String,
    val start: Double = 0.0,
    val height: Int? = null,
    val audio: Int? = null,
    val subtitle_burn: Int? = null,
    val audio_offset_ms: Long? = null,
    val block_budget_secs: Double? = null,
    val caps: DeviceCaps,
)

@Serializable
data class OpticalProgressRequest(
    val drive_id: String,
    val media_generation: String,
    val session_id: String,
    val angle: Int = 1,
    val position_ms: Long,
    val duration_ms: Long? = null,
    val audio: JsonElement? = null,
    val subtitle: JsonElement? = null,
    val recorded_at_ms: Long? = null,
)

@Serializable
data class OpticalEjectRequest(
    val media_generation: String,
    val session_id: String? = null,
    val stop_active: Boolean = false,
)
