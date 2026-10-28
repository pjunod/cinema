package tv.plurx.app.data

import kotlinx.serialization.Serializable

@Serializable internal data class SharedCaptionAsset(val asset_id: String, val profile_id: String, val model_id: String,
    val model_sha256: String, val content_revision: String, val audio_index: Long, val spoken_language: String,
    val output_language: String, val task: String, val duration_ms: Long)
@Serializable internal data class SharedCaptionCatalog(val version: Int, val assets: List<SharedCaptionAsset>, val next_asset: String? = null)
@Serializable internal data class SharedCaptionCoverage(val start_ms: Long, val end_ms: Long)
@Serializable internal data class SharedCaptionManifest(val version: Int, val asset_id: String, val content_revision: String,
    val profile_id: String, val audio_index: Long, val spoken_language: String, val output_language: String, val task: String,
    val duration_ms: Long, val manifest_revision: String, val state: String, val reason: String? = null,
    val snapshot: CaptionSnapshot, val coverage: List<SharedCaptionCoverage>, val segments: List<CaptionSegmentRef>)
