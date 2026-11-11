package tv.plurx.app.data

import kotlinx.serialization.Serializable

/** Wire counters are canonical decimal strings, never floating point numbers. */
@Serializable data class CaptionPermissions(val read: Boolean, val demand: Boolean, val stop: Boolean, val resume: Boolean)
@Serializable data class CaptionCapability(val supported: Boolean, val reason: String? = null)
@Serializable data class CaptionAudio(val index: Long, val audio_ordinal: Long? = null, val source_index: Long? = null,
    val capability: CaptionCapability? = null, val normalized_language: String? = null, val language: String? = null, val title: String? = null, val default: Boolean = false)
@Serializable data class CaptionCoreWindow(val core_ms: Long)
@Serializable data class CaptionProfile(val profile_id: String, val model_id: String, val task: String,
    val output_language: String? = null, val input_languages: List<String>, val label: String? = null, val default: Boolean? = null,
    val model_sha256: String? = null, val timeline_contract: String? = null, val window: CaptionCoreWindow? = null)
@Serializable data class CaptionAsset(val asset_id: String, val operation_id: String, val source_size: String,
    val source_mtime: String, val profile_id: String, val audio_index: Long, val spoken_language: String,
    val output_language: String, val content_revision: String, val timeline_contract: String)
@Serializable data class CaptionOptions(val version: Int, val file_id: String, val source_size: String, val source_mtime: String,
    val duration_ms: Long, val source_origin_ms: Long? = null, val timeline_contract: String,
    val audio_tracks: List<CaptionAudio>, val models: List<CaptionProfile>, val capability: CaptionCapability,
    val permissions: CaptionPermissions, val assets: List<CaptionAsset>, val assets_next_cursor: String? = null, val server_now_ms: Long, val default_profile_id: String? = null)
@Serializable data class CaptionAssetPage(val version: Int, val assets: List<CaptionAsset> = emptyList(),
    val entries: List<CaptionAsset> = emptyList(), val next_cursor: String? = null)
@Serializable data class CaptionCreate(val version: Int, val request_id: String, val intent_expires_at_ms: Long,
    val source_size: String, val source_mtime: String, val audio_index: Long, val spoken_language: String,
    val output_language: String, val task: String, val model_id: String, val profile_id: String,
    val playback_id: String, val position_ms: Long, val playback_rate: Double, val paused: Boolean, val continuity_id: String? = null)
@Serializable data class CaptionDemand(val version: Int, val request_id: String, val intent_expires_at_ms: Long,
    val expected_operation_epoch: String, val demand_generation: String, val playback_id: String,
    val position_ms: Long, val playback_rate: Double, val paused: Boolean, val continuity_id: String? = null)
@Serializable data class CaptionRelease(val version: Int, val request_id: String, val intent_expires_at_ms: Long,
    val expected_operation_epoch: String, val demand_generation: String, val playback_id: String)
@Serializable data class CaptionStop(val version: Int, val request_id: String, val intent_expires_at_ms: Long, val expected_operation_epoch: String)
@Serializable data class CaptionRetry(val scope: String, val version: Int, val request_id: String, val intent_expires_at_ms: Long,
    val expected_operation_epoch: String, val expected_view_revision: String, val expected_state: String,
    val playback_id: String, val position_ms: Long, val playback_rate: Double, val paused: Boolean, val continuity_id: String? = null)
@Serializable data class CaptionRangeRetry(val version: Int, val request_id: String, val intent_expires_at_ms: Long,
    val expected_operation_epoch: String, val expected_operation_state: String, val core_index: Long,
    val expected_range_revision: String, val expected_range_state: String, val playback_id: String,
    val position_ms: Long, val playback_rate: Double, val paused: Boolean, val scope: String, val continuity_id: String? = null)
@Serializable data class CaptionFailedRange(val core_index: Long, val range_revision: String,
    val reason: String? = null, val retryable: Boolean, val attempts: Long)
@Serializable data class CaptionFailedRangePage(val version: Int, val operation_id: String,
    val operation_epoch: String, val view_revision: String, val entries: List<CaptionFailedRange>, val next_cursor: String? = null)
@Serializable data class CaptionReceipt(val version: Int, val outcome: String, val operation_id: String,
    val operation_epoch: String, val view_revision: String, val state: String? = null, val asset_id: String? = null,
    val demand_id: String? = null, val demand_generation: String? = null, val server_now_ms: Long, val continuity_id: String? = null)
@Serializable data class CaptionSourceCheck(val state: String, val bytes_read: String, val total_bytes: String)
@Serializable data class CaptionJob(val phase: String, val reason: String? = null, val state: String? = null, val core_index: Long? = null)
@Serializable data class CaptionOperation(val version: Int, val operation_id: String, val operation_epoch: String,
    val view_revision: String, val state: String, val reason: String? = null, val retryable: Boolean,
    val asset_id: String? = null, val content_revision: String? = null, val source_size: String, val source_mtime: String, val audio_index: Long,
    val task: String, val spoken_language: String, val output_language: String, val profile_id: String,
    val source_check: CaptionSourceCheck, val active_job: CaptionJob? = null, val server_now_ms: Long,
    val failed_ranges: List<CaptionFailedRange> = emptyList(), val failed_ranges_next_cursor: String? = null,
    val file_id: String? = null, val model_id: String? = null, val model_sha256: String? = null, val timeline_contract: String? = null)
@Serializable data class CaptionSnapshot(val lease_id: String, val revision: String, val expires_in_ms: Long)
@Serializable data class CaptionOperationView(val operation_id: String, val epoch: String, val state: String, val reason: String? = null)
@Serializable data class CaptionSegmentRef(val segment_id: String, val digest: String, val start_ms: Long, val end_ms: Long)
@Serializable data class CaptionManifest(val version: Int, val asset_id: String, val file_id: String, val content_revision: String,
    val profile_id: String, val audio_index: Long, val spoken_language: String, val output_language: String,
    val timeline_contract: String, val manifest_revision: String, val view_revision: String,
    val operation: CaptionOperationView, val snapshot: CaptionSnapshot, val segments: List<CaptionSegmentRef>,
    val segment_next_cursor: String? = null)
@Serializable data class CaptionSegmentPage(val version: Int, val asset_id: String, val snapshot_revision: String,
    val entries: List<CaptionSegmentRef>, val next_cursor: String? = null)
@Serializable data class CaptionCue(val cue_id: String, val start_ms: Long, val end_ms: Long, val text: String)
@Serializable data class CaptionSegment(val version: Int, val asset_id: String, val segment_id: String, val start_ms: Long,
    val end_ms: Long, val cues: List<CaptionCue>, val provenance: CaptionProvenance)
@Serializable data class CaptionProvenance(val profile_id: String, val source_sha256: String, val operation_epoch: String, val model_sha256: String? = null)
@Serializable data class CaptionSettings(val version: Int, val revision: String, val enabled: Boolean,
    val model_id: String? = null, val profile_id: String? = null, val preferred_spoken_language: String? = null, val server_now_ms: Long)
@Serializable data class CaptionSettingsWrite(val version: Int, val request_id: String, val intent_expires_at_ms: Long,
    val expected_revision: String, val enabled: Boolean, val model_id: String, val profile_id: String, val preferred_spoken_language: String)
@Serializable data class CaptionModel(val id: String, val label: String, val download_bytes: Long)
@Serializable data class CaptionWorkerCapability(val profile_id: String, val status: String, val reason: String? = null)
@Serializable data class CaptionWorker(val node_id: String, val label: String, val current: Boolean, val eligible: Boolean,
    val expires_at_ms: Long, val capabilities: List<CaptionWorkerCapability>)
@Serializable data class CaptionCatalog(val version: Int, val models: List<CaptionModel>, val profiles: List<CaptionProfile>,
    val workers: List<CaptionWorker>, val server_now_ms: Long, val default_profile_id: String? = null)
@Serializable data class CaptionModelRequest(val version: Int, val request_id: String, val intent_expires_at_ms: Long,
    val purpose: String, val profile_id: String, val node_ids: List<String>)
@Serializable data class CaptionModelOperation(val node_id: String, val job_id: String? = null, val outcome: String)
@Serializable data class CaptionModelReceipt(val operations: List<CaptionModelOperation>)
@Serializable data class CaptionModelJob(val state: String, val phase: String, val reason: String? = null)
