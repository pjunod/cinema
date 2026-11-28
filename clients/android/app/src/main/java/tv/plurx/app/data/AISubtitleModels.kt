package tv.plurx.app.data

import kotlinx.serialization.Serializable

@Serializable
data class AISubtitleOptions(
    val allowed: Boolean,
    val route: String? = null,
    val label: String? = null,
    val existing_index: Long? = null,
    val search_online: Boolean = false,
    val alternatives: List<AISubtitleChoice> = emptyList(),
    val operation: AISubtitleOperation? = null,
)

@Serializable
data class AISubtitleChoice(val route: String, val label: String)

@Serializable
data class AISubtitleOperation(
    val id: String,
    val state: String,
    val progress: Double,
    val track_id: String? = null,
    val track_index: Long? = null,
    val route: String? = null,
) {
    val active: Boolean get() = state == "queued" || state == "working"
}

@Serializable
data class AISubtitleStart(
    val request_id: String,
    val target: String = "en",
    val route: String = "auto",
    val playback_id: String? = null,
    val position_ms: Long? = null,
    val quality: String? = null,
    val audio_index: Long? = null,
    val spoken_language: String? = null,
)

@Serializable
data class AISubtitleViewer(val playback_id: String, val position_ms: Long, val playback_rate: Double)

@Serializable
data class AISubtitleWindow(
    val state: String,
    val track_id: String? = null,
    val ready_from_ms: Long? = null,
    val ready_to_ms: Long,
    val cues: List<AISubtitleCue>,
)

@Serializable
data class AISubtitleCue(val start_ms: Long, val end_ms: Long, val text: String)

@Serializable
data class AISubtitlePreferences(val language: String, val mode: String)

@Serializable
data class AISubtitleSettings(
    val version: Int,
    val revision: String,
    val download_match: Boolean,
    val translate_text: Boolean,
    val from_audio: Boolean,
    val ahead_of_time: Boolean,
    val who: String,
    val quality: String,
    val readiness: AISubtitleReadiness? = null,
) {
    fun update() = AISubtitleSettingsWrite(version, revision, download_match, translate_text,
        from_audio, ahead_of_time, who, quality)
}

@Serializable
data class AISubtitleSettingsWrite(
    val version: Int,
    val expected_revision: String,
    val download_match: Boolean,
    val translate_text: Boolean,
    val from_audio: Boolean,
    val ahead_of_time: Boolean,
    val who: String,
    val quality: String,
)

@Serializable
data class AISubtitleReadiness(val nodes: List<AISubtitleNode>, val summary: AISubtitleSummary)

@Serializable
data class AISubtitleNode(val label: String, val state: String, val reason: String? = null)

@Serializable
data class AISubtitleSummary(val nodes: Int, val ready: Int)

@Serializable
data class AISubtitleSearch(val results: List<AISubtitleMatch> = emptyList())

@Serializable
data class AISubtitleMatch(val file_id: Long, val release: String, val language: String)

@Serializable
data class AISubtitleDownload(val provider_file_id: Long, val language: String)

@Serializable
data class AISubtitleDownloaded(val subtitle_index: Long)

@Serializable
data class AISubtitleTiming(val offset_ms: Long)
