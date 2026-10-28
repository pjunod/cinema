package tv.plurx.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

@Serializable data class SubtitleTextConfig(val version: Int, val revision: String, val enabled: Boolean,
    val model_id: String, val target_language: String)
@Serializable data class SubtitleTextConfigWrite(val version: Int, val expected_revision: String, val enabled: Boolean, val model_id: String)
@Serializable data class SubtitleTextSource(val index: Long, val language: String? = null, val title: String? = null, val text_eligible: Boolean, val downloaded_subtitle_id: String? = null)
@Serializable data class SubtitleTextWorker(val node_id: String, val label: String, val boot_id: String,
    val model_id: String, val status: String, val reason: String? = null)
@Serializable data class SubtitleTextPermissions(val read: Boolean, val create: Boolean)
@Serializable data class SubtitleTextOptions(val version: Int, val file_id: String, val source_size: String,
    val source_mtime: String, val subtitles: List<SubtitleTextSource>, val configuration: SubtitleTextConfig,
    val model_id: String, val workers: List<SubtitleTextWorker>, val permissions: SubtitleTextPermissions,
    val server_now_ms: Long, val input_languages: List<String> = emptyList())
@Serializable data class SubtitleTextCatalog(val version: Int, val catalog: JsonElement,
    val workers: List<SubtitleTextWorker>, val server_now_ms: Long)
@Serializable data class SubtitleTextModelRequest(val version: Int, val request_id: String, val node_id: String,
    val boot_id: String, val model_id: String)
@Serializable data class SubtitleTextRequest(val version: Int, val request_id: String, val source_size: String,
    val source_mtime: String, val source_language: String, val target_language: String, val model_id: String)
@Serializable data class SubtitleTextJobReceipt(val version: Int, val job_id: String, val state: String)
@Serializable data class SubtitleTextRetry(val version: Int, val request_id: String)
@Serializable data class SubtitleTextProgress(val completed: Long, val total: Long)
@Serializable data class SubtitleTextResult(val file_id: String, val subtitle_index: Long, val downloaded_subtitle_id: String, val language: String)
@Serializable data class SubtitleTextJob(val version: Int, val job_id: String, val state: String,
    val phase: String? = null, val reason: String? = null, val retryable: Boolean,
    val progress: SubtitleTextProgress, val result: SubtitleTextResult? = null)
